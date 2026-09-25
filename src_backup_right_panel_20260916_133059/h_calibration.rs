use crate::app::App;
use crate::durant::{DurantModel, H_CALIBRATION_FILENAME, HCalibration, MIN_ACTIVE_WEEKS};
use crate::player::PlayerId;
use crate::stats::{PlayerNineCatStats, PlayerWeeklyStats, StatsBundle};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::Instant;

const SCHEMA_VERSION: u32 = 1;
const TEAM_SIZE: usize = 13;
const DEFAULT_SAMPLES: usize = 20_000;
const DEFAULT_SEED: u64 = 0xB1D0_B04D_2026_0036;
const MIN_CALIBRATION_WEEKS: usize = 4;
const MIN_FINAL_TEST_WEEKS: usize = 4;
const BINS: usize = 10;

#[derive(Debug, Clone)]
struct Setup {
    model: DurantModel,
    train_weeks: Vec<u32>,
    calibration_weeks: Vec<u32>,
    test_weeks: Vec<u32>,
    calibration_rows: Vec<PlayerWeeklyStats>,
    test_rows: Vec<PlayerWeeklyStats>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    raw_h: f64,
    outcome: f64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct BinSummary {
    lower: f64,
    upper: f64,
    count: usize,
    mean_prediction: f64,
    observed_rate: f64,
    calibration_error: f64,
}

#[derive(Debug, Clone, Serialize)]
struct Metrics {
    count: usize,
    mean_prediction: f64,
    observed_rate: f64,
    brier: f64,
    log_loss: f64,
    ece: f64,
    max_bin_error: f64,
    bins: Vec<BinSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct Report {
    schema_version: u32,
    experiment: String,
    source_season: String,
    draft_season: String,
    method: String,
    slope: f64,
    train_weeks: Vec<u32>,
    calibration_weeks: Vec<u32>,
    final_test_weeks: Vec<u32>,
    calibration_samples: usize,
    final_test_samples: usize,
    calibration_raw: Metrics,
    calibration_fitted: Metrics,
    final_raw: Metrics,
    final_calibrated: Metrics,
    wall_time_seconds: f64,
    notes: Vec<String>,
}

#[derive(Debug, Clone)]
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn index(&mut self, upper: usize) -> usize {
        if upper <= 1 {
            0
        } else {
            (self.next_u64() as usize) % upper
        }
    }
}

pub fn print_plan(app: &App) {
    let weeks = unique_weeks(&app.stats.weekly);
    println!();
    println!("========================================================================");
    println!("H PROBABILITY CALIBRATION — v36");
    println!("========================================================================");
    println!("available weeks : {:?}", weeks);
    println!("samples/phase   : {}", requested_samples());
    println!("current slope   : {:.6}", app.durant.h_calibration_slope());
    println!();
    println!("Method:");
    println!("  1. fit raw historical DURANT/H on the earliest viable training prefix;");
    println!("  2. reserve >= {MIN_CALIBRATION_WEEKS} later weeks only to fit ONE symmetric");
    println!("     log-odds slope a: Hcal = logistic(a * logit(Hraw));");
    println!("  3. keep >= {MIN_FINAL_TEST_WEEKS} final weeks completely untouched;");
    println!("  4. compare raw vs calibrated ECE/Brier/log-loss on those final weeks;");
    println!("  5. save {H_CALIBRATION_FILENAME}; runtime loads it on the NEXT launch.");
    println!();
    println!("The transform is strictly monotonic, so strategy/player ordering is unchanged.");
    println!("Only the probability scale and probability differences are corrected.");
}

pub fn fit_validate_and_save(app: &App) -> Result<()> {
    let started = Instant::now();
    let samples = requested_samples();
    let seed = requested_seed();
    let setup = build_setup(app)?;

    println!();
    println!("========================================================================");
    println!("H PROBABILITY CALIBRATION — v36");
    println!("========================================================================");
    println!("train       : {:?}", setup.train_weeks);
    println!("calibration : {:?}", setup.calibration_weeks);
    println!("FINAL test  : {:?}", setup.test_weeks);
    println!(
        "Q={} | {} ACTIVE-WEEK samples/phase",
        setup.model.reference_size, samples
    );

    let calibration_rows = weekly_rows_by_week(&setup.calibration_rows);
    let test_rows = weekly_rows_by_week(&setup.test_rows);

    println!("generating calibration samples ...");
    let fit_samples = generate_samples(
        &setup.model,
        &setup.calibration_weeks,
        &calibration_rows,
        samples,
        seed ^ 0xCA11_BA7E,
    )?;
    if fit_samples.len() < samples / 2 {
        bail!(
            "only {} calibration samples could be formed",
            fit_samples.len()
        );
    }

    let slope = fit_symmetric_logit_slope(&fit_samples);
    let calibration = HCalibration::from_slope(slope);

    println!("fitted symmetric logit slope a = {:.6}", slope);
    println!("generating untouched FINAL test samples ...");
    let final_samples = generate_samples(
        &setup.model,
        &setup.test_weeks,
        &test_rows,
        samples,
        seed ^ 0xF1A1_7E57,
    )?;
    if final_samples.len() < samples / 2 {
        bail!(
            "only {} final-test samples could be formed",
            final_samples.len()
        );
    }

    let fit_raw = metrics(&fit_samples, HCalibration::default());
    let fit_cal = metrics(&fit_samples, calibration);
    let final_raw = metrics(&final_samples, HCalibration::default());
    let final_cal = metrics(&final_samples, calibration);

    let output_dir = app.stats.cache_dir.join("durant");
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("creating {}", output_dir.display()))?;

    let artifact_path = output_dir.join(H_CALIBRATION_FILENAME);
    fs::write(&artifact_path, serde_json::to_string_pretty(&calibration)?)
        .with_context(|| format!("writing {}", artifact_path.display()))?;

    let report = Report {
        schema_version: SCHEMA_VERSION,
        experiment: "symmetric logit calibration of raw independent-category H".to_string(),
        source_season: app.stats.source_season.clone(),
        draft_season: app.stats.draft_season.clone(),
        method: "H_cal = logistic(a * logit(H_raw)); no intercept".to_string(),
        slope,
        train_weeks: setup.train_weeks.clone(),
        calibration_weeks: setup.calibration_weeks.clone(),
        final_test_weeks: setup.test_weeks.clone(),
        calibration_samples: fit_samples.len(),
        final_test_samples: final_samples.len(),
        calibration_raw: fit_raw.clone(),
        calibration_fitted: fit_cal.clone(),
        final_raw: final_raw.clone(),
        final_calibrated: final_cal.clone(),
        wall_time_seconds: started.elapsed().as_secs_f64(),
        notes: vec![
            "The slope is fitted only on ACTIVE-WEEK matchups; it calibrates the H core conditional on the rostered players producing an observed weekly row. It does not solve injury/schedule availability uncertainty.".to_string(),
            "The final-test weeks are never used to fit the slope.".to_string(),
            "The transform is symmetric around 50% and strictly monotonic, so it preserves all raw-H option ordering and complement symmetry.".to_string(),
            "Runtime applies the transform after raw H has been averaged across opponents. This preserves the current optimizer ordering exactly.".to_string(),
        ],
    };

    let report_path = output_dir.join("h_calibration_v36_report.json");
    fs::write(&report_path, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("writing {}", report_path.display()))?;

    println!();
    print_metrics("CALIBRATION raw", &fit_raw);
    print_metrics("CALIBRATION fitted", &fit_cal);
    println!();
    println!("UNTOUCHED FINAL TEST");
    print_metrics("raw H", &final_raw);
    print_metrics("calibrated H", &final_cal);
    println!();
    println!("artifact : {}", artifact_path.display());
    println!("report   : {}", report_path.display());
    println!("wall time: {:.2}s", started.elapsed().as_secs_f64());
    println!();
    println!("Restart BirdBoard to load the new calibration slope.");

    Ok(())
}

fn print_metrics(label: &str, m: &Metrics) {
    println!(
        "  {:18} n={:5} | ECE {:5.2} pp | max {:5.2} pp | Brier {:.4} | log-loss {:.4}",
        label,
        m.count,
        m.ece * 100.0,
        m.max_bin_error * 100.0,
        m.brier,
        m.log_loss,
    );
}

fn build_setup(app: &App) -> Result<Setup> {
    let weeks = unique_weeks(&app.stats.weekly);
    let required_tail = MIN_CALIBRATION_WEEKS + MIN_FINAL_TEST_WEEKS;
    if weeks.len() < MIN_ACTIVE_WEEKS + required_tail {
        bail!(
            "only {} weeks available; calibration needs enough training history plus {} calibration/test weeks",
            weeks.len(),
            required_tail
        );
    }

    let league_teams = app.teams.len();
    if league_teams == 0 {
        bail!("H calibration needs at least one fantasy team");
    }

    let earliest_cut = (((weeks.len() as f64) * 0.65).ceil() as usize).max(MIN_ACTIVE_WEEKS + 1);
    let latest_cut = weeks.len().saturating_sub(required_tail);
    let mut last_error = None;

    for cut in earliest_cut..=latest_cut {
        let train_weeks = weeks[..cut].to_vec();
        let tail = &weeks[cut..];
        if tail.len() < required_tail {
            continue;
        }
        // Keep the final test at least four weeks and use the preceding tail
        // as calibration. With the current 25-week cache this becomes
        // train 1-17, calibrate 18-21, final test 22-25.
        let test_len = MIN_FINAL_TEST_WEEKS.max(tail.len() / 2);
        if tail.len().saturating_sub(test_len) < MIN_CALIBRATION_WEEKS {
            continue;
        }
        let split = tail.len() - test_len;
        let calibration_weeks = tail[..split].to_vec();
        let test_weeks = tail[split..].to_vec();

        let train_set = train_weeks.iter().copied().collect::<HashSet<_>>();
        let calibration_set = calibration_weeks.iter().copied().collect::<HashSet<_>>();
        let test_set = test_weeks.iter().copied().collect::<HashSet<_>>();

        let training_rows = app
            .stats
            .weekly
            .iter()
            .filter(|row| train_set.contains(&row.week))
            .cloned()
            .collect::<Vec<_>>();
        let calibration_rows = app
            .stats
            .weekly
            .iter()
            .filter(|row| calibration_set.contains(&row.week))
            .cloned()
            .collect::<Vec<_>>();
        let test_rows = app
            .stats
            .weekly
            .iter()
            .filter(|row| test_set.contains(&row.week))
            .cloned()
            .collect::<Vec<_>>();

        let training_players = aggregate_players_from_weekly(&app.stats.players, &training_rows);
        let training_bundle = StatsBundle {
            draft_season: format!("{}-h-calibration-train", app.stats.draft_season),
            source_season: app.stats.source_season.clone(),
            cache_dir: app.stats.cache_dir.clone(),
            players: training_players,
            weekly: training_rows,
        };

        match DurantModel::from_historical_stats_for_validation(
            &training_bundle,
            league_teams,
            TEAM_SIZE,
        ) {
            Ok(model) => {
                return Ok(Setup {
                    model,
                    train_weeks,
                    calibration_weeks,
                    test_weeks,
                    calibration_rows,
                    test_rows,
                });
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }

    bail!(
        "could not construct leakage-free three-way H calibration split; last error: {}",
        last_error.unwrap_or_else(|| "unknown".to_string())
    )
}

fn generate_samples(
    model: &DurantModel,
    weeks: &[u32],
    rows_by_week: &HashMap<u32, HashMap<PlayerId, PlayerWeeklyStats>>,
    requested: usize,
    seed: u64,
) -> Result<Vec<Sample>> {
    let mut rng = XorShift64::new(seed);
    let mut samples = Vec::with_capacity(requested);
    let needed = TEAM_SIZE * 2;

    for i in 0..requested {
        let week = weeks[i % weeks.len()];
        let Some(week_rows) = rows_by_week.get(&week) else {
            continue;
        };
        let pool = model
            .reference_players
            .iter()
            .filter(|id| week_rows.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        if pool.len() < needed {
            continue;
        }
        let Some((own, opp)) = sample_disjoint_rosters(&pool, &mut rng) else {
            continue;
        };
        let Some((_, raw_h)) = model.validation_complete_matchup_prediction(&own, &opp) else {
            continue;
        };
        let outcome = actual_matchup_outcome(&own, &opp, week_rows);
        samples.push(Sample { raw_h, outcome });
    }

    Ok(samples)
}

fn fit_symmetric_logit_slope(samples: &[Sample]) -> f64 {
    // One-parameter logistic regression with no intercept. Newton-Raphson is
    // convex here and converges in a handful of iterations.
    let mut a = 1.0_f64;
    for _ in 0..50 {
        let mut gradient = 0.0;
        let mut hessian = 0.0;
        for sample in samples {
            let x = logit(sample.raw_h);
            let q = logistic(a * x);
            gradient += (q - sample.outcome) * x;
            hessian += q * (1.0 - q) * x * x;
        }
        if hessian <= 1.0e-12 {
            break;
        }
        let step = gradient / hessian;
        let next = (a - step).clamp(0.05, 3.0);
        if (next - a).abs() < 1.0e-10 {
            a = next;
            break;
        }
        a = next;
    }
    a
}

fn metrics(samples: &[Sample], calibration: HCalibration) -> Metrics {
    let mut prediction_sum = 0.0;
    let mut observed_sum = 0.0;
    let mut brier_sum = 0.0;
    let mut log_loss_sum = 0.0;
    let mut bin_count = [0usize; BINS];
    let mut bin_pred = [0.0f64; BINS];
    let mut bin_obs = [0.0f64; BINS];

    for sample in samples {
        let p = calibration
            .apply(sample.raw_h)
            .clamp(1.0e-12, 1.0 - 1.0e-12);
        prediction_sum += p;
        observed_sum += sample.outcome;
        brier_sum += (p - sample.outcome).powi(2);
        log_loss_sum += -(sample.outcome * p.ln() + (1.0 - sample.outcome) * (1.0 - p).ln());
        let bin = ((p * BINS as f64).floor() as usize).min(BINS - 1);
        bin_count[bin] += 1;
        bin_pred[bin] += p;
        bin_obs[bin] += sample.outcome;
    }

    let n = samples.len().max(1) as f64;
    let mut ece = 0.0;
    let mut max_bin_error: f64 = 0.0;
    let mut bins = Vec::with_capacity(BINS);
    for i in 0..BINS {
        let lower = i as f64 / BINS as f64;
        let upper = (i + 1) as f64 / BINS as f64;
        if bin_count[i] == 0 {
            bins.push(BinSummary {
                lower,
                upper,
                ..BinSummary::default()
            });
            continue;
        }
        let bn = bin_count[i] as f64;
        let mean_prediction = bin_pred[i] / bn;
        let observed_rate = bin_obs[i] / bn;
        let error = (mean_prediction - observed_rate).abs();
        ece += error * bn / n;
        max_bin_error = max_bin_error.max(error);
        bins.push(BinSummary {
            lower,
            upper,
            count: bin_count[i],
            mean_prediction,
            observed_rate,
            calibration_error: error,
        });
    }

    Metrics {
        count: samples.len(),
        mean_prediction: prediction_sum / n,
        observed_rate: observed_sum / n,
        brier: brier_sum / n,
        log_loss: log_loss_sum / n,
        ece,
        max_bin_error,
        bins,
    }
}

fn sample_disjoint_rosters(
    pool: &[PlayerId],
    rng: &mut XorShift64,
) -> Option<(Vec<PlayerId>, Vec<PlayerId>)> {
    let needed = TEAM_SIZE * 2;
    if pool.len() < needed {
        return None;
    }
    let mut indices = (0..pool.len()).collect::<Vec<_>>();
    for i in 0..needed {
        let j = i + rng.index(pool.len() - i);
        indices.swap(i, j);
    }
    let own = indices[..TEAM_SIZE]
        .iter()
        .map(|&i| pool[i].clone())
        .collect();
    let opp = indices[TEAM_SIZE..needed]
        .iter()
        .map(|&i| pool[i].clone())
        .collect();
    Some((own, opp))
}

fn actual_matchup_outcome(
    own_roster: &[PlayerId],
    opponent_roster: &[PlayerId],
    rows: &HashMap<PlayerId, PlayerWeeklyStats>,
) -> f64 {
    let own = aggregate_actual(own_roster, rows);
    let opp = aggregate_actual(opponent_roster, rows);
    let own_values = [
        own.fg_pct(),
        own.ft_pct(),
        own.threes,
        own.points,
        own.rebounds,
        own.assists,
        own.steals,
        own.blocks,
        own.turnovers,
    ];
    let opp_values = [
        opp.fg_pct(),
        opp.ft_pct(),
        opp.threes,
        opp.points,
        opp.rebounds,
        opp.assists,
        opp.steals,
        opp.blocks,
        opp.turnovers,
    ];
    let mut wins = 0usize;
    let mut losses = 0usize;
    for cat in 0..9 {
        let ord = if cat == 8 {
            compare_lower_is_better(own_values[cat], opp_values[cat])
        } else {
            compare_higher_is_better(own_values[cat], opp_values[cat])
        };
        if ord > 0 {
            wins += 1;
        }
        if ord < 0 {
            losses += 1;
        }
    }
    if wins > losses {
        1.0
    } else if losses > wins {
        0.0
    } else {
        0.5
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ActualTotals {
    fgm: f64,
    fga: f64,
    ftm: f64,
    fta: f64,
    threes: f64,
    points: f64,
    rebounds: f64,
    assists: f64,
    steals: f64,
    blocks: f64,
    turnovers: f64,
}

impl ActualTotals {
    fn fg_pct(self) -> f64 {
        ratio(self.fgm, self.fga)
    }
    fn ft_pct(self) -> f64 {
        ratio(self.ftm, self.fta)
    }
}

fn aggregate_actual(
    roster: &[PlayerId],
    rows: &HashMap<PlayerId, PlayerWeeklyStats>,
) -> ActualTotals {
    let mut total = ActualTotals::default();
    for id in roster {
        let Some(row) = rows.get(id) else {
            continue;
        };
        total.fgm += row.fgm;
        total.fga += row.fga;
        total.ftm += row.ftm;
        total.fta += row.fta;
        total.threes += row.threes;
        total.points += row.points;
        total.rebounds += row.rebounds;
        total.assists += row.assists;
        total.steals += row.steals;
        total.blocks += row.blocks;
        total.turnovers += row.turnovers;
    }
    total
}

fn compare_higher_is_better(a: f64, b: f64) -> i8 {
    if (a - b).abs() <= 1.0e-12 {
        0
    } else if a > b {
        1
    } else {
        -1
    }
}

fn compare_lower_is_better(a: f64, b: f64) -> i8 {
    if (a - b).abs() <= 1.0e-12 {
        0
    } else if a < b {
        1
    } else {
        -1
    }
}

fn logit(p: f64) -> f64 {
    let p = p.clamp(1.0e-12, 1.0 - 1.0e-12);
    (p / (1.0 - p)).ln()
}

fn logistic(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let ez = z.exp();
        ez / (1.0 + ez)
    }
}

fn weekly_rows_by_week(
    rows: &[PlayerWeeklyStats],
) -> HashMap<u32, HashMap<PlayerId, PlayerWeeklyStats>> {
    let mut by_week = HashMap::<u32, HashMap<PlayerId, PlayerWeeklyStats>>::new();
    for row in rows {
        by_week
            .entry(row.week)
            .or_default()
            .insert(row.player_id.clone(), row.clone());
    }
    by_week
}

fn unique_weeks(rows: &[PlayerWeeklyStats]) -> Vec<u32> {
    let mut weeks = rows.iter().map(|row| row.week).collect::<Vec<_>>();
    weeks.sort_unstable();
    weeks.dedup();
    weeks
}

fn aggregate_players_from_weekly(
    originals: &[PlayerNineCatStats],
    weekly: &[PlayerWeeklyStats],
) -> Vec<PlayerNineCatStats> {
    #[derive(Debug, Clone, Default)]
    struct Agg {
        games: u32,
        fgm: f64,
        fga: f64,
        ftm: f64,
        fta: f64,
        threes: f64,
        points: f64,
        rebounds: f64,
        assists: f64,
        steals: f64,
        blocks: f64,
        turnovers: f64,
    }
    let original_by_id = originals
        .iter()
        .map(|p| (p.player_id.clone(), p))
        .collect::<HashMap<_, _>>();
    let mut aggs = HashMap::<PlayerId, Agg>::new();
    for row in weekly {
        let a = aggs.entry(row.player_id.clone()).or_default();
        a.games += row.games;
        a.fgm += row.fgm;
        a.fga += row.fga;
        a.ftm += row.ftm;
        a.fta += row.fta;
        a.threes += row.threes;
        a.points += row.points;
        a.rebounds += row.rebounds;
        a.assists += row.assists;
        a.steals += row.steals;
        a.blocks += row.blocks;
        a.turnovers += row.turnovers;
    }
    aggs.into_iter()
        .filter_map(|(id, a)| {
            let original = original_by_id.get(&id)?;
            if a.games == 0 {
                return None;
            }
            let g = a.games as f64;
            Some(PlayerNineCatStats {
                player_id: id,
                player_name: original.player_name.clone(),
                team: original.team.clone(),
                games: a.games,
                minutes_pg: original.minutes_pg,
                fgm_pg: a.fgm / g,
                fga_pg: a.fga / g,
                fg_pct: ratio(a.fgm, a.fga),
                ftm_pg: a.ftm / g,
                fta_pg: a.fta / g,
                ft_pct: ratio(a.ftm, a.fta),
                fgm_total: a.fgm,
                fga_total: a.fga,
                ftm_total: a.ftm,
                fta_total: a.fta,
                threes_pg: a.threes / g,
                points_pg: a.points / g,
                rebounds_pg: a.rebounds / g,
                assists_pg: a.assists / g,
                steals_pg: a.steals / g,
                blocks_pg: a.blocks / g,
                turnovers_pg: a.turnovers / g,
            })
        })
        .collect()
}

fn ratio(num: f64, den: f64) -> f64 {
    if den > 0.0 { num / den } else { 0.0 }
}

fn requested_samples() -> usize {
    std::env::var("BIRDBOARD_H_CALIBRATION_SAMPLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v >= 1000)
        .unwrap_or(DEFAULT_SAMPLES)
}

fn requested_seed() -> u64 {
    std::env::var("BIRDBOARD_H_CALIBRATION_SEED")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SEED)
}
