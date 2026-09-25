use crate::data::load_players;
use crate::data::load_teams;
use crate::data::save_players;
use crate::data::save_teams;
use crate::data::validate_data;
use crate::draft::DraftError;
use crate::draft::DraftMode;
use crate::draft::DraftPick;
use crate::durant::{
    AuctionConfig, AuctionDurantScore, DurantModel, DurantRosterPlan, DynamicDurantScore,
    MarketAdvantageScore, MarketBoard, MarketValue,
};
use crate::player::{Player, PlayerId};
use crate::projection::{PlayerProjection, ProjectionBook};
use crate::stats::StatsBundle;
use crate::strategy::{
    Build, ReplacementGroup, active_build, load_builds, load_replacements, replacement_for_player,
};
use crate::strategy_bank::{RuntimeStrategyBank, load_runtime_bank};
use crate::team::FantasyTeam;
use crate::team::TeamId;

use anyhow::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

const DURANT_PLAYER_POOL_LIMIT: usize = 200;

pub enum Screen {
    Home,
    Draft,
    Rosters,
    Strategy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Preparation,
    LiveDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    Browse,
    Edit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingEditCommand {
    None,
    Delete,
}

#[derive(Debug)]
pub struct PlayerRegister {
    pub player: Player,
    pub original_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerFormMode {
    Add,
    Edit(PlayerId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerFormField {
    Name,
    ShortName,
    Position,
    ProjectedValue,
}

impl PlayerFormField {
    fn next(self) -> Self {
        match self {
            Self::Name => Self::ShortName,
            Self::ShortName => Self::Position,
            Self::Position => Self::ProjectedValue,
            Self::ProjectedValue => Self::Name,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Name => Self::ProjectedValue,
            Self::ShortName => Self::Name,
            Self::Position => Self::ShortName,
            Self::ProjectedValue => Self::Position,
        }
    }
}

#[derive(Debug)]
pub struct PlayerForm {
    pub mode: PlayerFormMode,
    pub active_field: PlayerFormField,
    pub name: String,
    pub short_name: String,
    pub position: String,
    pub projected_value: String,
    pub error: Option<String>,
}

impl PlayerForm {
    fn active_value_mut(&mut self) -> &mut String {
        match self.active_field {
            PlayerFormField::Name => &mut self.name,
            PlayerFormField::ShortName => &mut self.short_name,
            PlayerFormField::Position => &mut self.position,
            PlayerFormField::ProjectedValue => &mut self.projected_value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionField {
    Fgm,
    Fga,
    Ftm,
    Fta,
    Threes,
    Points,
    Rebounds,
    Assists,
    Steals,
    Blocks,
    Turnovers,
}

impl ProjectionField {
    pub const ALL: [Self; 11] = [
        Self::Fgm,
        Self::Fga,
        Self::Ftm,
        Self::Fta,
        Self::Threes,
        Self::Points,
        Self::Rebounds,
        Self::Assists,
        Self::Steals,
        Self::Blocks,
        Self::Turnovers,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Fgm => "FGM",
            Self::Fga => "FGA",
            Self::Ftm => "FTM",
            Self::Fta => "FTA",
            Self::Threes => "3PM",
            Self::Points => "PTS",
            Self::Rebounds => "REB",
            Self::Assists => "AST",
            Self::Steals => "STL",
            Self::Blocks => "BLK",
            Self::Turnovers => "TO",
        }
    }

    fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0);
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone)]
pub struct ProjectionForm {
    pub player_id: PlayerId,
    pub player_name: String,
    pub team: String,
    pub minutes_pg: f64,
    pub active_field: ProjectionField,
    pub values: [String; 11],
    pub error: Option<String>,
}

impl ProjectionForm {
    pub fn value(&self, field: ProjectionField) -> &str {
        let index = ProjectionField::ALL
            .iter()
            .position(|candidate| *candidate == field)
            .unwrap_or(0);
        &self.values[index]
    }

    fn active_value_mut(&mut self) -> &mut String {
        let index = ProjectionField::ALL
            .iter()
            .position(|candidate| *candidate == self.active_field)
            .unwrap_or(0);
        &mut self.values[index]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamInputMode {
    Add,
    Edit(TeamId),
}

#[derive(Debug)]
pub struct TeamInput {
    pub mode: TeamInputMode,
    pub value: String,
    pub error: Option<String>,
}

pub struct App {
    pub running: bool,
    pub screen: Screen,
    pub session_phase: SessionPhase,
    pub interaction_mode: InteractionMode,

    pub players: Vec<Player>,
    pub curated_player_ids: HashSet<PlayerId>,
    /// Runtime canonical ID -> ID kept in players.csv. This lets BirdBoard use
    /// canonical stats IDs internally without silently breaking legacy build/
    /// replacement files that still reference the old curated IDs.
    curated_persisted_ids: HashMap<PlayerId, PlayerId>,
    pub selected_player: Option<usize>,

    pub teams: Vec<FantasyTeam>,
    pub selected_roster_team: Option<usize>,
    pub user_team_id: TeamId,

    pub stats: StatsBundle,
    pub projections: ProjectionBook,
    pub durant: DurantModel,
    pub market_board: MarketBoard,
    pub strategy_bank: Option<RuntimeStrategyBank>,
    pub live_roster_plan: Option<DurantRosterPlan>,
    pub live_advantage_board: Vec<MarketAdvantageScore>,
    pub live_rank_movement: HashMap<PlayerId, i16>,
    pub live_board_ms: Option<u128>,
    pub live_board_error: Option<String>,
    pub nomination_advice: Option<AuctionDurantScore>,
    pub nomination_advice_ms: Option<u128>,
    pub nomination_advice_error: Option<String>,
    pub projection_form: Option<ProjectionForm>,
    pub home_phase_selection: SessionPhase,
    pub live_refresh_pending: bool,
    pub live_analysis_dirty: bool,

    pub draft_picks: Vec<DraftPick>,
    pub draft_price_input: String,
    pub draft_mode: DraftMode,
    pub selected_team: Option<usize>,

    pub search_query: String,

    pub builds: Vec<Build>,
    pub replacements: Vec<ReplacementGroup>,
    pub selected_replacement: usize,

    pub pending_edit_command: PendingEditCommand,

    pub player_register: Option<PlayerRegister>,
    pub player_form: Option<PlayerForm>,
    pub data_dirty: bool,

    pub team_input: Option<TeamInput>,
    pub teams_dirty: bool,

    pub edit_status: Option<String>,
}

impl App {
    pub fn new(stats: StatsBundle) -> Result<App> {
        let mut players = load_players("data/players.csv")?;
        let mut original_curated_ids_by_name = HashMap::<String, PlayerId>::new();
        for player in &players {
            original_curated_ids_by_name
                .entry(normalized_player_name(&player.name))
                .or_insert_with(|| player.id.clone());
        }

        // players.csv predates the stats pipeline and may contain legacy IDs
        // for players who now have canonical Basketball-Reference/stat IDs.
        // Resolve those aliases by full player name BEFORE we build DURANT or
        // supplement the top-200 pool. This prevents one human player from
        // appearing twice (e.g. a legacy Tatum row plus tatumja01).
        let player_id_aliases = canonicalize_curated_player_ids(&mut players, &stats);
        let curated_player_ids = players
            .iter()
            .map(|player| player.id.clone())
            .collect::<HashSet<_>>();
        let curated_persisted_ids = players
            .iter()
            .map(|player| {
                let persisted = original_curated_ids_by_name
                    .get(&normalized_player_name(&player.name))
                    .cloned()
                    .unwrap_or_else(|| player.id.clone());
                (player.id.clone(), persisted)
            })
            .collect::<HashMap<_, _>>();

        let mut teams = load_teams("data/teams.csv")?;
        let projections = ProjectionBook::from_stats(&stats)?;
        let durant = DurantModel::from_stats(&stats, teams.len(), 13)?;
        let strategy_bank = load_runtime_bank(&stats.draft_season)?;

        // players.csv remains the source of manually curated players.  At
        // runtime, supplement it with the top DURANT-scored historical players
        // so Preparation/Live have a useful ~200-player statistical pool.
        // Existing rows always win, preserving their short names/positions.
        ensure_top_durant_players(&mut players, &durant);

        players.sort_by(|a, b| {
            let a_score = durant
                .score_for(&a.id)
                .map(|score| score.total)
                .unwrap_or(f64::NEG_INFINITY);
            let b_score = durant
                .score_for(&b.id)
                .map(|score| score.total)
                .unwrap_or(f64::NEG_INFINITY);
            b_score.total_cmp(&a_score)
        });

        let selected_player = if players.is_empty() { None } else { Some(0) };

        let user_team_id = teams
            .iter()
            .find(|team| team.is_user)
            .map(|team| team.id)
            .or_else(|| {
                teams
                    .iter()
                    .find(|team| team.id == TeamId(1))
                    .map(|team| team.id)
            })
            .or_else(|| teams.first().map(|team| team.id))
            .unwrap_or(TeamId(0));

        if let Some(team) = teams.iter_mut().find(|team| team.id == user_team_id) {
            team.is_user = true;
        }

        let selected_roster_team = if teams.is_empty() { None } else { Some(0) };

        let draft_picks = Vec::new();
        let mut builds = load_builds("data/builds.toml")?;
        let mut replacements = load_replacements("data/replacements.toml")?;
        remap_strategy_player_ids(&mut builds, &mut replacements, &player_id_aliases);

        validate_data(&players, &teams, &builds, &replacements)?;

        let mut app = App {
            running: true,
            screen: Screen::Home,
            session_phase: SessionPhase::Preparation,
            interaction_mode: InteractionMode::Browse,

            players,
            curated_player_ids,
            curated_persisted_ids,
            selected_player,

            teams,
            selected_roster_team,
            user_team_id,

            stats,
            projections,
            durant,
            market_board: MarketBoard::empty(),
            strategy_bank,
            live_roster_plan: None,
            live_advantage_board: Vec::new(),
            live_rank_movement: HashMap::new(),
            live_board_ms: None,
            live_board_error: None,
            nomination_advice: None,
            nomination_advice_ms: None,
            nomination_advice_error: None,
            projection_form: None,
            home_phase_selection: SessionPhase::Preparation,
            live_refresh_pending: false,
            live_analysis_dirty: true,

            draft_picks,
            draft_price_input: String::new(),
            draft_mode: DraftMode::BrowsingPlayers,
            selected_team: None,

            search_query: String::new(),

            builds,
            replacements,
            selected_replacement: 0,

            pending_edit_command: PendingEditCommand::None,

            player_register: None,
            player_form: None,
            data_dirty: false,

            team_input: None,
            teams_dirty: false,

            edit_status: None,
        };

        app.refresh_market_board();
        Ok(app)
    }

    pub fn quit(&mut self) {
        self.running = false;
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        /*
         * Input priority:
         *
         * 1. Player form
         * 2. Team-name input
         * 3. Player search
         * 4. Draft-screen edit commands
         * 5. Roster-screen edit commands
         * 6. Normal/global commands
         */

        if self.player_form.is_some() {
            self.handle_player_form_key(key);
            return;
        }

        if self.projection_form.is_some() {
            self.handle_projection_form_key(key);
            return;
        }

        if self.team_input.is_some() {
            self.handle_team_input_key(key);
            return;
        }

        if matches!(&self.screen, Screen::Draft)
            && matches!(&self.draft_mode, DraftMode::SearchingPlayer)
        {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.search_query.clear();
                    self.draft_mode = DraftMode::BrowsingPlayers;
                }

                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.update_search_selection();
                }

                KeyCode::Char(character) => {
                    self.search_query.push(character);
                    self.update_search_selection();
                }

                _ => {}
            }

            return;
        }

        if matches!(&self.screen, Screen::Draft)
            && matches!(&self.draft_mode, DraftMode::BrowsingPlayers)
            && matches!(&self.interaction_mode, InteractionMode::Edit)
        {
            match key.code {
                KeyCode::Char('/') => {
                    self.pending_edit_command = PendingEditCommand::None;

                    self.search_query.clear();
                    self.draft_mode = DraftMode::SearchingPlayer;
                }

                KeyCode::Char('d') => match self.pending_edit_command {
                    PendingEditCommand::None => {
                        self.pending_edit_command = PendingEditCommand::Delete;
                    }

                    PendingEditCommand::Delete => {
                        self.cut_selected_player();
                        self.pending_edit_command = PendingEditCommand::None;
                    }
                },

                KeyCode::Char('p') => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.paste_player_after();
                }

                KeyCode::Char('P') => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.paste_player_before();
                }

                KeyCode::Char('j') => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.select_next();
                }

                KeyCode::Char('k') => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.select_previous();
                }

                KeyCode::Enter => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.open_edit_player_form();
                }

                KeyCode::Char('a') => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.open_add_player_form();
                }

                KeyCode::Char('s') => {
                    self.pending_edit_command = PendingEditCommand::None;

                    let status = match self.save_player_board() {
                        Ok(()) => String::from("Saved data/players.csv"),
                        Err(error) => format!("SAVE FAILED: {error}"),
                    };

                    self.edit_status = Some(status);
                }

                KeyCode::Char('E') | KeyCode::Esc => {
                    self.pending_edit_command = PendingEditCommand::None;

                    // Do not leave edit mode while a player is cut.
                    if self.player_register.is_none() {
                        self.interaction_mode = InteractionMode::Browse;
                    }
                }

                _ => {
                    // An unrelated key cancels a pending first `d`.
                    self.pending_edit_command = PendingEditCommand::None;
                }
            }

            return;
        }

        if matches!(&self.screen, Screen::Rosters)
            && matches!(&self.interaction_mode, InteractionMode::Edit)
        {
            match key.code {
                KeyCode::Tab => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.select_next_roster_team();
                }

                KeyCode::BackTab => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.select_previous_roster_team();
                }

                KeyCode::Enter => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.open_edit_team_input();
                }

                KeyCode::Char('a') => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.open_add_team_input();
                }

                KeyCode::Char('c') => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.mark_selected_team_as_user();
                }

                KeyCode::Char('d') => match self.pending_edit_command {
                    PendingEditCommand::None => {
                        self.pending_edit_command = PendingEditCommand::Delete;
                    }

                    PendingEditCommand::Delete => {
                        self.remove_selected_team();
                        self.pending_edit_command = PendingEditCommand::None;
                    }
                },

                KeyCode::Char('s') => {
                    self.pending_edit_command = PendingEditCommand::None;

                    let status = match self.save_team_data() {
                        Ok(()) => String::from("Saved data/teams.csv"),
                        Err(error) => format!("SAVE FAILED: {error}"),
                    };

                    self.edit_status = Some(status);
                }

                KeyCode::Char('E') | KeyCode::Esc => {
                    self.pending_edit_command = PendingEditCommand::None;
                    self.interaction_mode = InteractionMode::Browse;
                    self.edit_status = None;
                }

                _ => {
                    self.pending_edit_command = PendingEditCommand::None;
                }
            }

            return;
        }

        if matches!(&self.screen, Screen::Home) {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.select_home_phase(SessionPhase::LiveDraft);
                    return;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.select_home_phase(SessionPhase::Preparation);
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('q') if !matches!(&self.draft_mode, DraftMode::SearchingPlayer) => {
                self.quit();
            }

            KeyCode::Char('h') if matches!(&self.draft_mode, DraftMode::BrowsingPlayers) => {
                self.screen = Screen::Home;
            }

            KeyCode::Char('b') if matches!(&self.draft_mode, DraftMode::BrowsingPlayers) => {
                self.screen = Screen::Draft;
            }

            KeyCode::Char('r')
                if matches!(&self.draft_mode, DraftMode::BrowsingPlayers)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft) =>
            {
                self.screen = Screen::Rosters;
            }

            KeyCode::Char('s')
                if matches!(&self.draft_mode, DraftMode::BrowsingPlayers)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft) =>
            {
                self.screen = Screen::Strategy;
            }

            KeyCode::Char('e')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers)
                    && matches!(&self.session_phase, SessionPhase::Preparation)
                    && matches!(&self.interaction_mode, InteractionMode::Browse) =>
            {
                self.open_projection_form();
            }

            KeyCode::Char('E')
                if matches!(&self.screen, Screen::Rosters) && self.editing_allowed() =>
            {
                self.toggle_edit_mode();
            }

            KeyCode::Esc
                if matches!(&self.interaction_mode, InteractionMode::Edit)
                    && matches!(&self.screen, Screen::Rosters) =>
            {
                self.toggle_edit_mode();
            }

            KeyCode::Char('j')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.select_next();
            }

            KeyCode::Char('k')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.select_previous();
            }

            KeyCode::Char('j')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.select_next_team();
            }

            KeyCode::Char('k')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.select_previous_team();
            }

            KeyCode::Char('j')
                if matches!(&self.screen, Screen::Rosters)
                    && matches!(&self.interaction_mode, InteractionMode::Browse) =>
            {
                self.select_next_roster_team();
            }

            KeyCode::Char('k')
                if matches!(&self.screen, Screen::Rosters)
                    && matches!(&self.interaction_mode, InteractionMode::Browse) =>
            {
                self.select_previous_roster_team();
            }

            KeyCode::Char('j') if matches!(&self.screen, Screen::Strategy) => {
                self.select_next_replacement();
            }

            KeyCode::Char('k') if matches!(&self.screen, Screen::Strategy) => {
                self.select_previous_replacement();
            }

            KeyCode::Char(digit)
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft)
                    && digit.is_ascii_digit()
                    && self.draft_price_input.len() < 3 =>
            {
                self.draft_price_input.push(digit);
            }

            KeyCode::Backspace
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.draft_price_input.pop();
            }

            KeyCode::Esc
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.escape_drafting_selected_player();
            }

            KeyCode::Enter
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers)
                    && matches!(&self.interaction_mode, InteractionMode::Browse)
                    && let Some(player_index) = self.selected_player
                    && self
                        .draft_pick_for_player(&self.players[player_index].id)
                        .is_none() =>
            {
                self.begin_drafting_selected_player();
            }

            KeyCode::Enter
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.confirm_recorded_draft();
            }

            KeyCode::Char('/')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.search_query.clear();
                self.draft_mode = DraftMode::SearchingPlayer;
            }

            KeyCode::Char('u')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.session_phase, SessionPhase::LiveDraft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.undo_last_pick();
            }

            _ => {}
        }
    }

    fn select_home_phase(&mut self, phase: SessionPhase) {
        self.home_phase_selection = phase;
        self.session_phase = phase;

        // Mode selection on Home is immediate. Entering a different mode also
        // leaves transient editor/draft interaction behind without touching
        // the cached live analysis.
        self.interaction_mode = InteractionMode::Browse;
        self.pending_edit_command = PendingEditCommand::None;
        self.projection_form = None;
        self.player_form = None;
        self.team_input = None;
        self.draft_mode = DraftMode::BrowsingPlayers;
        self.selected_team = None;
        self.draft_price_input.clear();
        self.edit_status = None;

        match phase {
            SessionPhase::Preparation => {
                self.live_refresh_pending = false;
                // Preparation only changes presentation. Do NOT throw away the
                // expensive strategy/H result merely because the user wants to
                // inspect or edit the static board. If a projection is actually
                // edited, refresh_market_board() marks the live cache dirty.
                self.sort_players_by_static_durant();
            }
            SessionPhase::LiveDraft => {
                if self.live_analysis_dirty || self.live_advantage_board.is_empty() {
                    // First entry (or a genuinely changed model/draft state) is
                    // expensive, so defer one frame for visual feedback.
                    self.live_refresh_pending = true;
                } else {
                    // Pure Preparation -> Live navigation: reuse the cached
                    // strategy result and simply restore the live ordering.
                    self.live_refresh_pending = false;
                    let selected_id = self.selected_player_ref().map(|player| player.id.clone());
                    self.sort_players_by_live_advantage(selected_id.as_ref());
                }
            }
        }
    }

    /// Execute one deferred UI-visible job.
    ///
    /// The main loop draws once before calling this, which means switching to
    /// Live immediately shows feedback instead of looking frozen while the
    /// market-aware ΔH board is calculated.
    pub fn process_pending_work(&mut self) -> bool {
        if !self.live_refresh_pending {
            return false;
        }

        self.live_refresh_pending = false;
        self.refresh_market_board();
        true
    }

    pub fn select_next(&mut self) {
        if let Some(index) = self.selected_player
            && index + 1 < self.players.len()
        {
            self.selected_player = Some(index + 1);
        }
    }

    pub fn select_previous(&mut self) {
        if let Some(index) = self.selected_player
            && index > 0
        {
            self.selected_player = Some(index - 1);
        }
    }

    pub fn record_draft(
        &mut self,
        player_index: usize,
        team_index: usize,
        price: u8,
    ) -> Result<(), DraftError> {
        if self.players.get(player_index).is_none() {
            return Err(DraftError::InvalidPlayer);
        }

        if self.teams.get(team_index).is_none() {
            return Err(DraftError::InvalidTeam);
        }

        let player_id = self.players[player_index].id.clone();
        let team_id = self.teams[team_index].id;

        let player_already_drafted = self
            .draft_picks
            .iter()
            .any(|pick| pick.player_id == player_id);

        if player_already_drafted {
            return Err(DraftError::PlayerAlreadyDrafted);
        }

        let team = &self.teams[team_index];

        if price > team.budget {
            return Err(DraftError::InsufficientFunds);
        }

        self.teams[team_index].budget -= price;

        self.draft_picks.push(DraftPick {
            player_id,
            team_id,
            price,
        });

        self.session_phase = SessionPhase::LiveDraft;
        self.home_phase_selection = SessionPhase::LiveDraft;
        self.interaction_mode = InteractionMode::Browse;
        self.pending_edit_command = PendingEditCommand::None;
        self.player_form = None;
        self.team_input = None;
        self.refresh_market_board();

        // Once a sale is recorded, return attention to the best currently
        // available live-board option instead of leaving the cursor on a
        // drafted player that has moved to the bottom of the board.
        self.selected_player = self
            .players
            .iter()
            .position(|player| self.draft_pick_for_player(&player.id).is_none());

        Ok(())
    }

    pub fn begin_drafting_selected_player(&mut self) {
        if !matches!(self.session_phase, SessionPhase::LiveDraft) {
            return;
        }

        if self.selected_player.is_some() && !self.teams.is_empty() {
            self.refresh_nomination_advice();
            self.draft_mode = DraftMode::RecordingDraft;
            self.selected_team = Some(0);
            self.draft_price_input.clear();
        }
    }

    pub fn select_next_team(&mut self) {
        if self.teams.is_empty() {
            self.selected_team = None;
            return;
        }

        self.selected_team = match self.selected_team {
            Some(index) => Some((index + 1) % self.teams.len()),
            None => Some(0),
        };
    }

    pub fn select_previous_team(&mut self) {
        if self.teams.is_empty() {
            self.selected_team = None;
            return;
        }

        self.selected_team = match self.selected_team {
            Some(0) => Some(self.teams.len() - 1),
            Some(index) => Some(index - 1),
            None => Some(0),
        };
    }

    pub fn confirm_recorded_draft(&mut self) {
        let (Some(player_index), Some(team_index)) = (self.selected_player, self.selected_team)
        else {
            return;
        };

        let Ok(price) = self.draft_price_input.parse::<u8>() else {
            return;
        };

        if self.record_draft(player_index, team_index, price).is_ok() {
            self.draft_mode = DraftMode::BrowsingPlayers;
            self.selected_team = None;
            self.draft_price_input.clear();
        }
    }

    pub fn escape_drafting_selected_player(&mut self) {
        self.draft_mode = DraftMode::BrowsingPlayers;
        self.selected_team = None;
        self.draft_price_input.clear();
        self.nomination_advice = None;
        self.nomination_advice_ms = None;
        self.nomination_advice_error = None;
    }

    pub fn draft_pick_for_player(&self, player_id: &PlayerId) -> Option<&DraftPick> {
        self.draft_picks
            .iter()
            .find(|pick| &pick.player_id == player_id)
    }

    pub fn player_by_id(&self, player_id: &PlayerId) -> Option<&Player> {
        self.players.iter().find(|player| &player.id == player_id)
    }

    pub fn team_has_player(&self, team_id: TeamId, player_id: &PlayerId) -> bool {
        self.draft_picks
            .iter()
            .any(|pick| pick.team_id == team_id && &pick.player_id == player_id)
    }

    pub fn team_by_id(&self, team_id: TeamId) -> Option<&FantasyTeam> {
        self.teams.iter().find(|team| team.id == team_id)
    }

    pub fn current_build(&self) -> Option<&Build> {
        active_build(&self.builds, |player_id| {
            self.team_has_player(self.user_team_id, player_id)
        })
    }

    pub fn triggered_replacements(&self) -> Vec<&ReplacementGroup> {
        let Some(build) = self.current_build() else {
            return Vec::new();
        };

        build
            .target_players
            .iter()
            .filter(|player_id| {
                matches!(
                    self.draft_pick_for_player(*player_id),
                    Some(pick) if pick.team_id != self.user_team_id
                )
            })
            .filter_map(|player_id| replacement_for_player(&self.replacements, player_id))
            .collect()
    }

    pub fn select_next_replacement(&mut self) {
        let count = self.triggered_replacements().len();

        if count == 0 {
            return;
        }

        let current = self.selected_replacement % count;

        self.selected_replacement = (current + 1) % count;
    }

    pub fn select_previous_replacement(&mut self) {
        let count = self.triggered_replacements().len();

        if count == 0 {
            return;
        }

        let current = self.selected_replacement % count;

        self.selected_replacement = (current + count - 1) % count;
    }

    pub fn undo_last_pick(&mut self) -> bool {
        let Some(pick) = self.draft_picks.pop() else {
            return false;
        };

        let Some(team_index) = self.teams.iter().position(|team| team.id == pick.team_id) else {
            // Preserve the draft if its team cannot be found.
            self.draft_picks.push(pick);
            return false;
        };

        let Some(refunded_budget) = self.teams[team_index].budget.checked_add(pick.price) else {
            self.draft_picks.push(pick);
            return false;
        };

        self.teams[team_index].budget = refunded_budget;

        self.selected_player = self
            .players
            .iter()
            .position(|player| player.id == pick.player_id);

        self.selected_replacement = 0;
        self.refresh_market_board();

        true
    }

    pub fn selected_player_ref(&self) -> Option<&Player> {
        self.selected_player
            .and_then(|index| self.players.get(index))
    }

    pub fn projection_for(&self, player_id: &PlayerId) -> Option<&PlayerProjection> {
        self.projections.for_player(player_id)
    }

    pub fn market_value_for(&self, player_id: &PlayerId) -> Option<&MarketValue> {
        self.market_board.value_for(player_id)
    }

    pub fn durant_rank_for(&self, player_id: &PlayerId) -> Option<usize> {
        self.durant
            .scores
            .iter()
            .position(|score| &score.player_id == player_id)
            .map(|index| index + 1)
    }

    pub fn own_roster_ids(&self) -> Vec<PlayerId> {
        self.draft_picks
            .iter()
            .filter(|pick| pick.team_id == self.user_team_id)
            .map(|pick| pick.player_id.clone())
            .collect()
    }

    pub fn roster_ids_for_team(&self, team_id: TeamId) -> Vec<PlayerId> {
        self.draft_picks
            .iter()
            .filter(|pick| pick.team_id == team_id)
            .map(|pick| pick.player_id.clone())
            .collect()
    }

    pub fn immediate_fit_for(&self, player_id: &PlayerId) -> Option<f64> {
        if self.draft_pick_for_player(player_id).is_some() {
            return None;
        }
        self.durant.immediate_fit(&self.own_roster_ids(), player_id)
    }

    pub fn team_x_profile(&self, team_id: TeamId) -> [f64; 9] {
        self.durant
            .roster_x_profile(&self.roster_ids_for_team(team_id))
    }

    pub fn refresh_market_board(&mut self) {
        // Any caller reaching here has changed something relevant to market or
        // roster construction (projection, pick, undo, team budget/data, etc.).
        // The expensive live analysis is therefore stale until recomputed.
        self.live_analysis_dirty = true;

        let available = self
            .players
            .iter()
            .filter(|player| self.draft_pick_for_player(&player.id).is_none())
            .map(|player| player.id.clone())
            .collect::<Vec<_>>();

        let own_roster = self.own_roster_ids();
        let own_budget = self
            .team_by_id(self.user_team_id)
            .map(|team| team.budget as u16)
            .unwrap_or(AuctionConfig::default().starting_budget);

        let opponent_teams = self
            .teams
            .iter()
            .filter(|team| team.id != self.user_team_id)
            .collect::<Vec<_>>();

        let opponent_rosters = opponent_teams
            .iter()
            .map(|team| self.roster_ids_for_team(team.id))
            .collect::<Vec<_>>();
        let opponent_budgets = opponent_teams
            .iter()
            .map(|team| team.budget as u16)
            .collect::<Vec<_>>();

        self.market_board = self.durant.market_board(
            &own_roster,
            own_budget,
            &opponent_rosters,
            &opponent_budgets,
            &available,
            AuctionConfig::default(),
        );

        if matches!(self.session_phase, SessionPhase::LiveDraft) {
            self.refresh_live_roster_plan();
            self.refresh_live_advantage_board();
            self.live_analysis_dirty = false;
        }

        self.nomination_advice = None;
        self.nomination_advice_ms = None;
        self.nomination_advice_error = None;
    }

    pub fn live_advantage_for(&self, player_id: &PlayerId) -> Option<&MarketAdvantageScore> {
        self.live_advantage_board
            .iter()
            .find(|score| &score.player_id == player_id)
    }

    pub fn live_board_rank_for(&self, player_id: &PlayerId) -> Option<usize> {
        self.live_advantage_board
            .iter()
            .position(|score| &score.player_id == player_id)
            .map(|index| index + 1)
    }

    pub fn live_rank_movement_for(&self, player_id: &PlayerId) -> Option<i16> {
        self.live_rank_movement.get(player_id).copied()
    }

    pub fn live_team_building_delta_for(&self, player_id: &PlayerId) -> Option<f64> {
        self.live_advantage_for(player_id)
            .map(|score| score.marginal_projected_matchup_win_probability)
    }

    /// Rank by the player's immediate H impact only, before future roster
    /// construction or budget allocation. This makes "great player, awkward
    /// build" visible separately from the final team-building rank.
    pub fn live_immediate_rank_for(&self, player_id: &PlayerId) -> Option<usize> {
        let target = self.live_advantage_for(player_id)?;

        Some(
            1 + self
                .live_advantage_board
                .iter()
                .filter(|other| {
                    other
                        .marginal_immediate_matchup_win_probability
                        .total_cmp(&target.marginal_immediate_matchup_win_probability)
                        .is_gt()
                        || (other.marginal_immediate_matchup_win_probability
                            == target.marginal_immediate_matchup_win_probability
                            && other
                                .buy_projected_matchup_win_probability
                                .total_cmp(&target.buy_projected_matchup_win_probability)
                                .is_gt())
                })
                .count(),
        )
    }

    /// Rank by the best budget-aware completed-roster H after buying the
    /// player at market. This is deliberately independent of Big Board order,
    /// because the Big Board itself is now sorted by NOW ΔH.
    pub fn live_projected_rank_for(&self, player_id: &PlayerId) -> Option<usize> {
        let target = self.live_advantage_for(player_id)?;

        Some(
            1 + self
                .live_advantage_board
                .iter()
                .filter(|other| {
                    other
                        .buy_projected_matchup_win_probability
                        .total_cmp(&target.buy_projected_matchup_win_probability)
                        .is_gt()
                        || (other.buy_projected_matchup_win_probability
                            == target.buy_projected_matchup_win_probability
                            && other
                                .marginal_projected_matchup_win_probability
                                .total_cmp(&target.marginal_projected_matchup_win_probability)
                                .is_gt())
                })
                .count(),
        )
    }

    /// Positive = player becomes MORE attractive after optimal construction.
    /// Negative = player is immediately strong but harder to build around.
    pub fn live_buildability_movement_for(&self, player_id: &PlayerId) -> Option<i16> {
        let immediate_rank = self.live_immediate_rank_for(player_id)?;
        let projected_rank = self.live_projected_rank_for(player_id)?;

        Some((immediate_rank as i16) - (projected_rank as i16))
    }

    pub fn live_board_evaluated_count(&self) -> usize {
        self.live_advantage_board.len()
    }

    pub fn runtime_strategy_count(&self) -> usize {
        self.strategy_bank
            .as_ref()
            .map_or(0, RuntimeStrategyBank::len)
    }

    pub fn runtime_strategy_source(&self) -> Option<&str> {
        self.strategy_bank
            .as_ref()
            .and_then(|bank| bank.source_path.file_name())
            .and_then(|name| name.to_str())
    }

    pub fn runtime_strategy_profile(&self) -> Option<&str> {
        self.strategy_bank
            .as_ref()
            .map(|bank| bank.search_profile.as_str())
    }

    pub fn refresh_live_roster_plan(&mut self) {
        self.live_roster_plan = None;

        let Some(bank) = &self.strategy_bank else {
            return;
        };
        if bank.is_empty() {
            return;
        }
        let weights = bank.weights.clone();

        let own_roster = self.own_roster_ids();
        let own_budget = self
            .team_by_id(self.user_team_id)
            .map(|team| team.budget as u16)
            .unwrap_or(AuctionConfig::default().starting_budget);

        let drafted = self
            .draft_picks
            .iter()
            .map(|pick| pick.player_id.clone())
            .collect::<HashSet<_>>();
        let candidates = self
            .players
            .iter()
            .filter(|player| !drafted.contains(&player.id))
            .filter(|player| self.durant.score_for(&player.id).is_some())
            .map(|player| player.id.clone())
            .collect::<Vec<_>>();

        let opponent_teams = self
            .teams
            .iter()
            .filter(|team| team.id != self.user_team_id)
            .collect::<Vec<_>>();
        let opponent_rosters = opponent_teams
            .iter()
            .map(|team| self.roster_ids_for_team(team.id))
            .collect::<Vec<_>>();
        let opponent_budgets = opponent_teams
            .iter()
            .map(|team| team.budget as u16)
            .collect::<Vec<_>>();

        self.live_roster_plan = self.durant.roster_plan_with_strategy_weights(
            &own_roster,
            own_budget,
            &opponent_rosters,
            &opponent_budgets,
            &candidates,
            &weights,
            AuctionConfig::default(),
        );
    }

    fn refresh_live_advantage_board(&mut self) {
        self.live_board_ms = None;
        self.live_board_error = None;

        let Some(bank) = &self.strategy_bank else {
            self.live_advantage_board.clear();
            self.live_rank_movement.clear();
            return;
        };
        if bank.is_empty() {
            self.live_advantage_board.clear();
            self.live_rank_movement.clear();
            return;
        }
        let weights = bank.weights.clone();

        let drafted = self
            .draft_picks
            .iter()
            .map(|pick| pick.player_id.clone())
            .collect::<HashSet<_>>();

        // MOVE compares NOW ΔH rank before and after the last pick,
        // after removing the newly drafted player from the old board.
        let previous_ranks = self
            .live_advantage_board
            .iter()
            .filter(|score| !drafted.contains(&score.player_id))
            .enumerate()
            .map(|(index, score)| (score.player_id.clone(), index + 1))
            .collect::<HashMap<_, _>>();

        let available_candidates = self
            .market_board
            .values
            .iter()
            .filter(|value| !drafted.contains(&value.player_id))
            .map(|value| value.player_id.clone())
            .collect::<Vec<_>>();

        let own_roster = self.own_roster_ids();
        let own_budget = self
            .team_by_id(self.user_team_id)
            .map(|team| team.budget as u16)
            .unwrap_or(AuctionConfig::default().starting_budget);
        // Visible live universe = top 200 static DURANT + every curated/manual
        // player with a usable DURANT score.  Do not pre-filter using the
        // Preparation/static market: Live computes its own opponent-competition
        // clearing price inside DURANT.
        let mut evaluated_candidates = Vec::<PlayerId>::new();
        let mut evaluated_seen = HashSet::<PlayerId>::new();

        let mut push_if_actionable = |player_id: &PlayerId| {
            if drafted.contains(player_id) || !evaluated_seen.insert(player_id.clone()) {
                return;
            }

            if self.durant.score_for(player_id).is_none() {
                return;
            }

            evaluated_candidates.push(player_id.clone());
        };

        for score in self.durant.scores.iter().take(DURANT_PLAYER_POOL_LIMIT) {
            push_if_actionable(&score.player_id);
        }
        for player in &self.players {
            if self.curated_player_ids.contains(&player.id)
                && self.durant.score_for(&player.id).is_some()
            {
                push_if_actionable(&player.id);
            }
        }

        let opponent_teams = self
            .teams
            .iter()
            .filter(|team| team.id != self.user_team_id)
            .collect::<Vec<_>>();
        let opponent_rosters = opponent_teams
            .iter()
            .map(|team| self.roster_ids_for_team(team.id))
            .collect::<Vec<_>>();
        let opponent_budgets = opponent_teams
            .iter()
            .map(|team| team.budget as u16)
            .collect::<Vec<_>>();

        let selected_id = self.selected_player_ref().map(|player| player.id.clone());
        let started = Instant::now();

        match self.durant.market_advantage_scores_with_strategy_weights(
            &own_roster,
            own_budget,
            &opponent_rosters,
            &opponent_budgets,
            &available_candidates,
            &evaluated_candidates,
            &weights,
            AuctionConfig::default(),
        ) {
            Ok(mut scores) => {
                self.live_board_ms = Some(started.elapsed().as_millis());

                // The Live Big Board is an orientation tool first: rank by
                // the player's IMMEDIATE contribution to the current roster.
                //
                // NOW ΔH = H(current roster + player) - H(current roster)
                //
                // FINAL ΔH and BUILD remain visible beside it, so the user can
                // immediately see when a great player is difficult/easy to
                // construct around without letting price/rollout effects hide
                // the player's direct impact.
                scores.sort_by(|a, b| {
                    b.marginal_immediate_matchup_win_probability
                        .total_cmp(&a.marginal_immediate_matchup_win_probability)
                        .then_with(|| {
                            b.buy_projected_matchup_win_probability
                                .total_cmp(&a.buy_projected_matchup_win_probability)
                        })
                        .then_with(|| {
                            b.marginal_projected_matchup_win_probability
                                .total_cmp(&a.marginal_projected_matchup_win_probability)
                        })
                });

                self.live_rank_movement = scores
                    .iter()
                    .enumerate()
                    .filter_map(|(index, score)| {
                        let previous_rank = previous_ranks.get(&score.player_id).copied()?;
                        let new_rank = index + 1;
                        let movement = (previous_rank as i16) - (new_rank as i16);
                        Some((score.player_id.clone(), movement))
                    })
                    .collect();

                self.live_advantage_board = scores;
                self.sort_players_by_live_advantage(selected_id.as_ref());
            }
            Err(error) => {
                self.live_board_ms = Some(started.elapsed().as_millis());
                self.live_board_error = Some(error.to_string());
                self.live_advantage_board.clear();
                self.live_rank_movement.clear();
                self.sort_players_by_static_durant();
            }
        }
    }

    fn sort_players_by_static_durant(&mut self) {
        let selected_id = self.selected_player_ref().map(|player| player.id.clone());

        self.players.sort_by(|a, b| {
            let a_score = self
                .durant
                .score_for(&a.id)
                .map(|score| score.total)
                .unwrap_or(f64::NEG_INFINITY);
            let b_score = self
                .durant
                .score_for(&b.id)
                .map(|score| score.total)
                .unwrap_or(f64::NEG_INFINITY);
            b_score.total_cmp(&a_score)
        });

        if let Some(selected_id) = selected_id {
            self.selected_player = self
                .players
                .iter()
                .position(|player| player.id == selected_id);
        }
    }

    fn sort_players_by_live_advantage(&mut self, selected_id: Option<&PlayerId>) {
        let drafted = self
            .draft_picks
            .iter()
            .map(|pick| pick.player_id.clone())
            .collect::<HashSet<_>>();

        let live_ranks = self
            .live_advantage_board
            .iter()
            .enumerate()
            .map(|(index, score)| (score.player_id.clone(), index))
            .collect::<HashMap<_, _>>();

        let static_ranks = self
            .durant
            .scores
            .iter()
            .enumerate()
            .map(|(index, score)| (score.player_id.clone(), index))
            .collect::<HashMap<_, _>>();

        self.players.sort_by(|a, b| {
            let a_drafted = drafted.contains(&a.id);
            let b_drafted = drafted.contains(&b.id);

            a_drafted.cmp(&b_drafted).then_with(|| {
                match (live_ranks.get(&a.id), live_ranks.get(&b.id)) {
                    (Some(a_rank), Some(b_rank)) => a_rank.cmp(b_rank),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => static_ranks
                        .get(&a.id)
                        .copied()
                        .unwrap_or(usize::MAX)
                        .cmp(&static_ranks.get(&b.id).copied().unwrap_or(usize::MAX)),
                }
            })
        });

        if let Some(selected_id) = selected_id {
            self.selected_player = self
                .players
                .iter()
                .position(|player| &player.id == selected_id);
        }
    }

    pub fn refresh_nomination_advice(&mut self) {
        self.nomination_advice = None;
        self.nomination_advice_ms = None;
        self.nomination_advice_error = None;

        let Some(player_id) = self.selected_player_ref().map(|player| player.id.clone()) else {
            return;
        };
        if self.draft_pick_for_player(&player_id).is_some() {
            return;
        }

        let Some(bank) = &self.strategy_bank else {
            self.nomination_advice_error = Some("No runtime strategy bank loaded".to_string());
            return;
        };
        if bank.is_empty() {
            self.nomination_advice_error = Some("Runtime strategy bank is empty".to_string());
            return;
        }
        let weights = bank.weights.clone();

        let own_roster = self.own_roster_ids();
        let own_budget = self
            .team_by_id(self.user_team_id)
            .map(|team| team.budget as u16)
            .unwrap_or(AuctionConfig::default().starting_budget);

        let drafted = self
            .draft_picks
            .iter()
            .map(|pick| pick.player_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let candidates = self
            .players
            .iter()
            .filter(|player| !drafted.contains(&player.id))
            .map(|player| player.id.clone())
            .collect::<Vec<_>>();

        let opponent_teams = self
            .teams
            .iter()
            .filter(|team| team.id != self.user_team_id)
            .collect::<Vec<_>>();
        let opponent_rosters = opponent_teams
            .iter()
            .map(|team| self.roster_ids_for_team(team.id))
            .collect::<Vec<_>>();
        let opponent_budgets = opponent_teams
            .iter()
            .map(|team| team.budget as u16)
            .collect::<Vec<_>>();

        let started = Instant::now();
        match self
            .durant
            .auction_candidate_analysis_with_strategy_weights(
                &own_roster,
                own_budget,
                &opponent_rosters,
                &opponent_budgets,
                &candidates,
                &player_id,
                &weights,
                AuctionConfig::default(),
            ) {
            Ok(advice) => {
                self.nomination_advice = advice;
                self.nomination_advice_ms = Some(started.elapsed().as_millis());
            }
            Err(error) => {
                self.nomination_advice_ms = Some(started.elapsed().as_millis());
                self.nomination_advice_error = Some(error.to_string());
            }
        }
    }

    pub fn open_projection_form(&mut self) {
        let Some(player) = self.selected_player_ref() else {
            return;
        };
        let Some(projection) = self.projection_for(&player.id) else {
            self.edit_status = Some("No projection data for selected player".to_string());
            return;
        };

        let values = [
            projection.fgm_pg,
            projection.fga_pg,
            projection.ftm_pg,
            projection.fta_pg,
            projection.threes_pg,
            projection.points_pg,
            projection.rebounds_pg,
            projection.assists_pg,
            projection.steals_pg,
            projection.blocks_pg,
            projection.turnovers_pg,
        ]
        .map(|value| format!("{value:.2}"));

        self.projection_form = Some(ProjectionForm {
            player_id: projection.player_id.clone(),
            player_name: projection.player_name.clone(),
            team: projection.team.clone(),
            minutes_pg: projection.minutes_pg,
            active_field: ProjectionField::Points,
            values,
            error: None,
        });
    }

    fn handle_projection_form_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.projection_form = None;
            }
            KeyCode::Char('j') | KeyCode::Tab | KeyCode::Down => {
                if let Some(form) = &mut self.projection_form {
                    form.active_field = form.active_field.next();
                    form.error = None;
                }
            }
            KeyCode::Char('k') | KeyCode::BackTab | KeyCode::Up => {
                if let Some(form) = &mut self.projection_form {
                    form.active_field = form.active_field.previous();
                    form.error = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(form) = &mut self.projection_form {
                    form.active_value_mut().pop();
                    form.error = None;
                }
            }
            KeyCode::Char('c') => {
                if let Some(form) = &mut self.projection_form {
                    form.active_value_mut().clear();
                    form.error = None;
                }
            }
            KeyCode::Char(character) if character.is_ascii_digit() || character == '.' => {
                if let Some(form) = &mut self.projection_form {
                    let value = form.active_value_mut();
                    if character != '.' || !value.contains('.') {
                        value.push(character);
                    }
                    form.error = None;
                }
            }
            KeyCode::Char('e') => {
                if let Err(error) = self.save_projection_form(true) {
                    if let Some(form) = &mut self.projection_form {
                        form.error = Some(error.to_string());
                    }
                }
            }
            KeyCode::Enter => {
                // Enter is intentionally an unadvertised shortcut: it performs
                // the same save/rebuild as a second `e`, but without a status
                // message. This keeps the preparation workflow centered on `e`.
                if let Err(error) = self.save_projection_form(false) {
                    if let Some(form) = &mut self.projection_form {
                        form.error = Some(error.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    fn save_projection_form(&mut self, show_status: bool) -> Result<()> {
        let Some(form) = self.projection_form.clone() else {
            return Ok(());
        };

        let parsed = form
            .values
            .iter()
            .map(|value| value.trim().parse::<f64>())
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let projection = PlayerProjection {
            player_id: form.player_id.clone(),
            player_name: form.player_name.clone(),
            team: form.team.clone(),
            minutes_pg: form.minutes_pg,
            fgm_pg: parsed[0],
            fga_pg: parsed[1],
            ftm_pg: parsed[2],
            fta_pg: parsed[3],
            threes_pg: parsed[4],
            points_pg: parsed[5],
            rebounds_pg: parsed[6],
            assists_pg: parsed[7],
            steals_pg: parsed[8],
            blocks_pg: parsed[9],
            turnovers_pg: parsed[10],
            is_manual_override: true,
        };

        self.projections.save_manual_override(&projection)?;
        self.durant = DurantModel::from_stats(&self.stats, self.teams.len(), 13)?;
        ensure_top_durant_players(&mut self.players, &self.durant);

        let selected_id = projection.player_id.clone();
        self.players.sort_by(|a, b| {
            let a_score = self
                .durant
                .score_for(&a.id)
                .map(|score| score.total)
                .unwrap_or(f64::NEG_INFINITY);
            let b_score = self
                .durant
                .score_for(&b.id)
                .map(|score| score.total)
                .unwrap_or(f64::NEG_INFINITY);
            b_score.total_cmp(&a_score)
        });
        self.selected_player = self
            .players
            .iter()
            .position(|player| player.id == selected_id);
        self.refresh_market_board();
        self.projection_form = None;
        self.edit_status = if show_status {
            Some(format!("Saved projection for {}", projection.player_name))
        } else {
            None
        };

        Ok(())
    }

    pub fn editing_allowed(&self) -> bool {
        matches!(self.session_phase, SessionPhase::Preparation)
    }

    pub fn toggle_edit_mode(&mut self) {
        if !self.editing_allowed() {
            return;
        }

        self.interaction_mode = match self.interaction_mode {
            InteractionMode::Browse => {
                if matches!(self.screen, Screen::Rosters)
                    && self.selected_roster_team.is_none()
                    && !self.teams.is_empty()
                {
                    self.selected_roster_team = Some(0);
                }

                InteractionMode::Edit
            }

            InteractionMode::Edit => InteractionMode::Browse,
        };

        self.pending_edit_command = PendingEditCommand::None;
        self.edit_status = None;
    }

    pub fn current_screen_is_editable(&self) -> bool {
        matches!(self.screen, Screen::Draft | Screen::Rosters)
    }

    pub fn cut_selected_player(&mut self) -> bool {
        if self.player_register.is_some() {
            return false;
        }

        let Some(player_index) = self.selected_player else {
            return false;
        };

        if player_index >= self.players.len() {
            return false;
        }

        let player = self.players.remove(player_index);

        self.player_register = Some(PlayerRegister {
            player,
            original_index: player_index,
        });

        self.selected_player = if self.players.is_empty() {
            None
        } else {
            Some(player_index.min(self.players.len() - 1))
        };

        self.data_dirty = true;
        self.edit_status = None;

        true
    }

    pub fn paste_player_after(&mut self) -> bool {
        let Some(register) = self.player_register.take() else {
            return false;
        };

        let insert_index = match self.selected_player {
            Some(player_index) => (player_index + 1).min(self.players.len()),
            None => 0,
        };

        self.players.insert(insert_index, register.player);

        self.selected_player = Some(insert_index);
        self.data_dirty = true;
        self.edit_status = None;

        true
    }

    pub fn paste_player_before(&mut self) -> bool {
        let Some(register) = self.player_register.take() else {
            return false;
        };

        let insert_index = match self.selected_player {
            Some(player_index) => player_index.min(self.players.len()),
            None => 0,
        };

        self.players.insert(insert_index, register.player);

        self.selected_player = Some(insert_index);
        self.data_dirty = true;
        self.edit_status = None;

        true
    }

    pub fn save_player_board(&mut self) -> Result<()> {
        validate_data(&self.players, &self.teams, &self.builds, &self.replacements)?;

        // A cut curated player that is still sitting in the register is a
        // confirmed deletion when the board is saved.
        if let Some(register) = &self.player_register {
            self.curated_player_ids.remove(&register.player.id);
            self.curated_persisted_ids.remove(&register.player.id);
        }

        // players.csv is the user's curated/manual layer. Runtime top-200
        // supplementation must never leak back into that file just because the
        // user saved unrelated edits.
        let curated_players = self
            .players
            .iter()
            .filter(|player| self.curated_player_ids.contains(&player.id))
            .cloned()
            .map(|mut player| {
                if let Some(persisted_id) = self.curated_persisted_ids.get(&player.id) {
                    player.id = persisted_id.clone();
                }
                player
            })
            .collect::<Vec<_>>();

        save_players("data/players.csv", &curated_players)?;

        self.data_dirty = false;
        self.player_register = None;
        self.refresh_market_board();

        Ok(())
    }

    pub fn select_next_roster_team(&mut self) {
        if self.teams.is_empty() {
            self.selected_roster_team = None;
            return;
        }

        self.selected_roster_team = Some(
            self.selected_roster_team
                .map_or(0, |index| (index + 1) % self.teams.len()),
        );
    }

    pub fn select_previous_roster_team(&mut self) {
        if self.teams.is_empty() {
            self.selected_roster_team = None;
            return;
        }

        self.selected_roster_team = Some(match self.selected_roster_team {
            Some(0) | None => self.teams.len() - 1,
            Some(index) => index - 1,
        });
    }

    pub fn mark_selected_team_as_user(&mut self) -> bool {
        let Some(team_index) = self.selected_roster_team else {
            return false;
        };

        let Some(team_id) = self.teams.get(team_index).map(|team| team.id) else {
            return false;
        };

        if team_id == self.user_team_id {
            return false;
        }

        for team in &mut self.teams {
            team.is_user = team.id == team_id;
        }

        self.user_team_id = team_id;
        self.teams_dirty = true;
        self.edit_status = None;
        self.refresh_market_board();

        true
    }

    pub fn remove_selected_team(&mut self) -> bool {
        if self.teams.len() <= 1 {
            self.edit_status = Some(String::from("The final team cannot be removed."));
            return false;
        }

        let Some(team_index) = self.selected_roster_team else {
            return false;
        };

        let Some(team) = self.teams.get(team_index) else {
            return false;
        };

        if team.id == self.user_team_id {
            self.edit_status = Some(String::from("Mark another team as yours first."));
            return false;
        }

        self.teams.remove(team_index);

        self.selected_roster_team = Some(team_index.min(self.teams.len() - 1));

        self.teams_dirty = true;
        self.edit_status = None;

        true
    }

    pub fn save_team_data(&mut self) -> Result<()> {
        validate_data(&self.players, &self.teams, &self.builds, &self.replacements)?;

        save_teams("data/teams.csv", &self.teams)?;

        self.teams_dirty = false;
        self.durant = DurantModel::from_stats(&self.stats, self.teams.len(), 13)?;
        ensure_top_durant_players(&mut self.players, &self.durant);
        self.refresh_market_board();

        Ok(())
    }

    pub fn dynamic_durant_scores(&self) -> Vec<DynamicDurantScore> {
        use std::collections::HashSet;

        let own_roster = self
            .draft_picks
            .iter()
            .filter(|pick| pick.team_id == self.user_team_id)
            .map(|pick| pick.player_id.clone())
            .collect::<Vec<_>>();

        let opponent_rosters = self
            .teams
            .iter()
            .filter(|team| team.id != self.user_team_id)
            .map(|team| {
                self.draft_picks
                    .iter()
                    .filter(|pick| pick.team_id == team.id)
                    .map(|pick| pick.player_id.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let drafted = self
            .draft_picks
            .iter()
            .map(|pick| pick.player_id.clone())
            .collect::<HashSet<_>>();

        let candidates = self
            .players
            .iter()
            .filter(|player| !drafted.contains(&player.id))
            .map(|player| player.id.clone())
            .collect::<Vec<_>>();

        match &self.strategy_bank {
            Some(bank) if !bank.is_empty() => self.durant.dynamic_scores_with_strategy_weights(
                &own_roster,
                &opponent_rosters,
                &candidates,
                &bank.weights,
            ),
            _ => self
                .durant
                .dynamic_scores(&own_roster, &opponent_rosters, &candidates),
        }
    }

    fn open_edit_player_form(&mut self) {
        let Some(player_index) = self.selected_player else {
            return;
        };

        let Some(player) = self.players.get(player_index) else {
            return;
        };

        self.edit_status = None;

        self.player_form = Some(PlayerForm {
            mode: PlayerFormMode::Edit(player.id.clone()),
            active_field: PlayerFormField::Name,
            name: player.name.clone(),
            short_name: player.short_name.clone().unwrap_or_default(),
            position: player.position.clone(),
            projected_value: player.projected_value.to_string(),
            error: None,
        });
    }

    fn open_add_player_form(&mut self) {
        self.edit_status = None;

        self.player_form = Some(PlayerForm {
            mode: PlayerFormMode::Add,
            active_field: PlayerFormField::Name,
            name: String::new(),
            short_name: String::new(),
            position: String::new(),
            projected_value: String::new(),
            error: None,
        });
    }

    fn handle_player_form_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.player_form = None;
            }

            KeyCode::Tab => {
                if let Some(form) = &mut self.player_form {
                    form.active_field = form.active_field.next();
                    form.error = None;
                }
            }

            KeyCode::BackTab => {
                if let Some(form) = &mut self.player_form {
                    form.active_field = form.active_field.previous();
                    form.error = None;
                }
            }

            KeyCode::Backspace => {
                if let Some(form) = &mut self.player_form {
                    form.active_value_mut().pop();
                    form.error = None;
                }
            }

            KeyCode::Enter => {
                self.apply_player_form();
            }

            KeyCode::Char(character) => {
                let Some(form) = &mut self.player_form else {
                    return;
                };

                if matches!(form.active_field, PlayerFormField::ProjectedValue) {
                    if character.is_ascii_digit() && form.projected_value.len() < 3 {
                        form.projected_value.push(character);
                    }
                } else {
                    form.active_value_mut().push(character);
                }

                form.error = None;
            }

            _ => {}
        }
    }

    fn apply_player_form(&mut self) {
        let Some(form) = self.player_form.as_ref() else {
            return;
        };

        let mode = form.mode.clone();
        let name = form.name.trim().to_string();
        let short_name = form.short_name.trim().to_string();
        let position = form.position.trim().to_string();
        let projected_value_input = form.projected_value.trim().to_string();

        if name.is_empty() {
            self.set_player_form_error("Player name cannot be empty.");
            return;
        }

        if position.is_empty() {
            self.set_player_form_error("Position cannot be empty.");
            return;
        }

        let Ok(projected_value) = projected_value_input.parse::<u8>() else {
            self.set_player_form_error("Projected value must be between 0 and 255.");
            return;
        };

        let short_name = if short_name.is_empty() {
            None
        } else {
            Some(short_name)
        };

        match mode {
            PlayerFormMode::Edit(player_id) => {
                // Editing player metadata makes this a deliberately curated row.
                self.curated_player_ids.insert(player_id.clone());

                let Some(player) = self
                    .players
                    .iter_mut()
                    .find(|player| player.id == player_id)
                else {
                    self.set_player_form_error("The selected player no longer exists.");
                    return;
                };

                player.name = name;
                player.short_name = short_name;
                player.position = position;
                player.projected_value = projected_value;
            }

            PlayerFormMode::Add => {
                let Some(player_id) = self.player_id_for_name(&name) else {
                    self.set_player_form_error(
                        "Could not find this player in the statistics dataset. ",
                    );
                    return;
                };

                let insert_index = match self.selected_player {
                    Some(index) => (index + 1).min(self.players.len()),
                    None => 0,
                };

                self.curated_player_ids.insert(player_id.clone());
                self.curated_persisted_ids
                    .entry(player_id.clone())
                    .or_insert_with(|| player_id.clone());

                self.players.insert(
                    insert_index,
                    Player {
                        id: player_id,
                        name,
                        short_name,
                        position,
                        projected_value,
                    },
                );

                self.selected_player = Some(insert_index);
            }
        }

        self.player_form = None;
        self.data_dirty = true;
        self.edit_status = None;
    }

    fn set_player_form_error(&mut self, message: impl Into<String>) {
        if let Some(form) = &mut self.player_form {
            form.error = Some(message.into());
        }
    }

    fn player_id_for_name(&self, name: &str) -> Option<PlayerId> {
        self.stats
            .players
            .iter()
            .find(|stats| stats.player_name.eq_ignore_ascii_case(name))
            .map(|stats| stats.player_id.clone())
    }

    fn open_edit_team_input(&mut self) {
        let Some(team_index) = self.selected_roster_team else {
            return;
        };

        let Some(team) = self.teams.get(team_index) else {
            return;
        };

        self.team_input = Some(TeamInput {
            mode: TeamInputMode::Edit(team.id),
            value: team.name.clone(),
            error: None,
        });

        self.edit_status = None;
    }

    fn open_add_team_input(&mut self) {
        self.team_input = Some(TeamInput {
            mode: TeamInputMode::Add,
            value: String::new(),
            error: None,
        });

        self.edit_status = None;
    }

    fn handle_team_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.team_input = None;
            }

            KeyCode::Backspace => {
                if let Some(input) = &mut self.team_input {
                    input.value.pop();
                    input.error = None;
                }
            }

            KeyCode::Enter => {
                self.apply_team_input();
            }

            KeyCode::Char(character) => {
                if let Some(input) = &mut self.team_input {
                    input.value.push(character);
                    input.error = None;
                }
            }

            _ => {}
        }
    }

    fn apply_team_input(&mut self) {
        let Some(input) = self.team_input.as_ref() else {
            return;
        };

        let mode = input.mode;
        let name = input.value.trim().to_string();

        if name.is_empty() {
            self.set_team_input_error("Team name cannot be empty.");
            return;
        }

        match mode {
            TeamInputMode::Edit(team_id) => {
                let Some(team) = self.teams.iter_mut().find(|team| team.id == team_id) else {
                    self.set_team_input_error("The selected team no longer exists.");
                    return;
                };

                team.name = name;
            }

            TeamInputMode::Add => {
                let Some(team_id) = self.next_available_team_id() else {
                    self.set_team_input_error("No unused team IDs remain.");
                    return;
                };

                self.teams.push(FantasyTeam {
                    id: team_id,
                    name,
                    budget: 200,
                    is_user: false,
                });

                self.selected_roster_team = Some(self.teams.len() - 1);
            }
        }

        self.team_input = None;
        self.teams_dirty = true;
        self.edit_status = None;
    }

    fn set_team_input_error(&mut self, message: impl Into<String>) {
        if let Some(input) = &mut self.team_input {
            input.error = Some(message.into());
        }
    }

    fn next_available_team_id(&self) -> Option<TeamId> {
        match self.teams.iter().map(|team| team.id.0).max() {
            Some(id) => id.checked_add(1).map(TeamId),
            None => Some(TeamId(0)),
        }
    }

    fn update_search_selection(&mut self) {
        if self.search_query.is_empty() {
            return;
        }

        let matcher = SkimMatcherV2::default();

        let best_match = self
            .players
            .iter()
            .enumerate()
            .filter_map(|(index, player)| {
                matcher
                    .fuzzy_match(&player.name, &self.search_query)
                    .map(|score| (index, score))
            })
            .max_by_key(|(_, score)| *score);

        if let Some((index, _score)) = best_match {
            self.selected_player = Some(index);
        }
    }
}

fn normalized_player_name(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

/// Migrate legacy/hand-authored player IDs onto the canonical IDs supplied by
/// the stats pipeline whenever the full player name identifies a unique match.
///
/// This is intentionally a runtime migration: old builds/replacement files may
/// still reference the legacy IDs, so we return an alias map and remap those
/// references before validation. players.csv keeps its persisted IDs on save
/// until we deliberately migrate the strategy files too.
fn canonicalize_curated_player_ids(
    players: &mut Vec<Player>,
    stats: &StatsBundle,
) -> HashMap<PlayerId, PlayerId> {
    let mut canonical_by_name = HashMap::<String, PlayerId>::new();
    let mut ambiguous_names = HashSet::<String>::new();

    for stat in &stats.players {
        let key = normalized_player_name(&stat.player_name);
        if let Some(existing) = canonical_by_name.get(&key) {
            if existing != &stat.player_id {
                ambiguous_names.insert(key);
            }
        } else {
            canonical_by_name.insert(key, stat.player_id.clone());
        }
    }

    for key in &ambiguous_names {
        canonical_by_name.remove(key);
    }

    let mut aliases = HashMap::<PlayerId, PlayerId>::new();
    for player in players.iter_mut() {
        let key = normalized_player_name(&player.name);
        if let Some(canonical_id) = canonical_by_name.get(&key)
            && &player.id != canonical_id
        {
            aliases.insert(player.id.clone(), canonical_id.clone());
            player.id = canonical_id.clone();
        }
    }

    // Collapse any contamination already present in players.csv. Prefer the
    // first curated row and only fill metadata that it is missing.
    let mut deduped = Vec::<Player>::with_capacity(players.len());
    let mut index_by_id = HashMap::<PlayerId, usize>::new();
    let mut index_by_name = HashMap::<String, usize>::new();

    for player in players.drain(..) {
        let name_key = normalized_player_name(&player.name);
        let existing_index = index_by_id
            .get(&player.id)
            .copied()
            .or_else(|| index_by_name.get(&name_key).copied());

        if let Some(index) = existing_index {
            let existing = &mut deduped[index];
            if existing.short_name.as_deref().map_or(true, str::is_empty)
                && player
                    .short_name
                    .as_deref()
                    .map_or(false, |name| !name.is_empty())
            {
                existing.short_name = player.short_name.clone();
            }
            if (existing.position.trim().is_empty() || existing.position == "—")
                && !player.position.trim().is_empty()
                && player.position != "—"
            {
                existing.position = player.position.clone();
            }
            existing.projected_value = existing.projected_value.max(player.projected_value);
            aliases.insert(player.id.clone(), existing.id.clone());
            continue;
        }

        let index = deduped.len();
        index_by_id.insert(player.id.clone(), index);
        index_by_name.insert(name_key, index);
        deduped.push(player);
    }

    *players = deduped;
    aliases
}

fn canonical_player_id(player_id: &PlayerId, aliases: &HashMap<PlayerId, PlayerId>) -> PlayerId {
    aliases
        .get(player_id)
        .cloned()
        .unwrap_or_else(|| player_id.clone())
}

fn remap_strategy_player_ids(
    builds: &mut [Build],
    replacements: &mut [ReplacementGroup],
    aliases: &HashMap<PlayerId, PlayerId>,
) {
    for build in builds {
        for player_id in &mut build.required_players {
            *player_id = canonical_player_id(player_id, aliases);
        }
        let mut seen_required = HashSet::new();
        build
            .required_players
            .retain(|player_id| seen_required.insert(player_id.clone()));

        for player_id in &mut build.target_players {
            *player_id = canonical_player_id(player_id, aliases);
        }
        let mut seen_targets = HashSet::new();
        build
            .target_players
            .retain(|player_id| seen_targets.insert(player_id.clone()));
    }

    for replacement in replacements {
        for player_id in &mut replacement.primary_players {
            *player_id = canonical_player_id(player_id, aliases);
        }
        let mut seen_primary = HashSet::new();
        replacement
            .primary_players
            .retain(|player_id| seen_primary.insert(player_id.clone()));

        for option in &mut replacement.alternatives {
            option.player_id = canonical_player_id(&option.player_id, aliases);
        }
        let primary_ids = replacement
            .primary_players
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let mut seen_alternatives = HashSet::new();
        replacement.alternatives.retain(|option| {
            !primary_ids.contains(&option.player_id)
                && seen_alternatives.insert(option.player_id.clone())
        });
    }
}

fn ensure_top_durant_players(players: &mut Vec<Player>, durant: &DurantModel) {
    let mut existing_ids = players
        .iter()
        .map(|player| player.id.clone())
        .collect::<HashSet<_>>();
    let mut existing_names = players
        .iter()
        .map(|player| normalized_player_name(&player.name))
        .collect::<HashSet<_>>();

    for score in durant.scores.iter().take(DURANT_PLAYER_POOL_LIMIT) {
        let name_key = normalized_player_name(&score.player_name);
        if existing_ids.contains(&score.player_id) || existing_names.contains(&name_key) {
            continue;
        }

        existing_ids.insert(score.player_id.clone());
        existing_names.insert(name_key);
        players.push(Player {
            id: score.player_id.clone(),
            name: score.player_name.clone(),
            short_name: None,
            // The historical stats cache currently has no position column.
            // Do not fabricate one. If this player is later curated manually,
            // the user's position metadata will persist in players.csv.
            position: "—".to_string(),
            projected_value: 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::player::PlayerId;
    use crate::stats::PlayerNineCatStats;
    use crate::team::TeamId;

    use super::*;
    use std::path::PathBuf;

    fn empty_stats() -> StatsBundle {
        StatsBundle {
            draft_season: String::from("2026-27"),
            source_season: String::from("2025-26"),
            cache_dir: PathBuf::new(),
            players: Vec::new(),
            weekly: Vec::new(),
        }
    }

    fn stat_player(id: &str, name: &str) -> PlayerNineCatStats {
        PlayerNineCatStats {
            player_id: PlayerId(id.to_string()),
            player_name: name.to_string(),
            team: String::new(),
            games: 0,
            minutes_pg: 0.0,
            fgm_pg: 0.0,
            fga_pg: 0.0,
            fg_pct: 0.0,
            ftm_pg: 0.0,
            fta_pg: 0.0,
            ft_pct: 0.0,
            fgm_total: 0.0,
            fga_total: 0.0,
            ftm_total: 0.0,
            fta_total: 0.0,
            threes_pg: 0.0,
            points_pg: 0.0,
            rebounds_pg: 0.0,
            assists_pg: 0.0,
            steals_pg: 0.0,
            blocks_pg: 0.0,
            turnovers_pg: 0.0,
        }
    }

    fn dummy_live_score(player_id: &str, player_name: &str) -> MarketAdvantageScore {
        MarketAdvantageScore {
            player_id: PlayerId(player_id.to_string()),
            player_name: player_name.to_string(),
            market_price: 10,
            current_matchup_win_probability: 0.50,
            current_category_win_probabilities: [0.5; 9],
            immediate_matchup_win_probability: 0.53,
            marginal_immediate_matchup_win_probability: 0.03,
            immediate_category_win_probabilities: [0.5; 9],
            marginal_projected_matchup_win_probability: 0.01,
            buy_projected_matchup_win_probability: 0.55,
            pass_projected_matchup_win_probability: 0.54,
            pass_projected_category_win_probabilities: [0.5; 9],
            buy_projected_category_win_probabilities: [0.5; 9],
            build_name: String::from("Balanced"),
            j_name: String::from("Balanced"),
            j_weights: [1.0; 9],
            projected_future_players: Vec::new(),
            projected_future_player_names: Vec::new(),
            projected_future_spend: 0,
            projected_budget_left: 0,
        }
    }

    fn test_app(players: Vec<Player>, selected_player: Option<usize>) -> App {
        let curated_player_ids = players
            .iter()
            .map(|player| player.id.clone())
            .collect::<HashSet<_>>();
        let curated_persisted_ids = players
            .iter()
            .map(|player| (player.id.clone(), player.id.clone()))
            .collect::<HashMap<_, _>>();

        App {
            running: true,
            screen: Screen::Draft,
            session_phase: SessionPhase::Preparation,
            interaction_mode: InteractionMode::Browse,

            players,
            curated_player_ids,
            curated_persisted_ids,

            stats: empty_stats(),
            projections: ProjectionBook::empty(),
            durant: DurantModel::empty(13, 13),
            market_board: MarketBoard::empty(),
            strategy_bank: None,
            live_roster_plan: None,
            live_advantage_board: Vec::new(),
            live_rank_movement: HashMap::new(),
            live_board_ms: None,
            live_board_error: None,
            nomination_advice: None,
            nomination_advice_ms: None,
            nomination_advice_error: None,
            projection_form: None,
            home_phase_selection: SessionPhase::Preparation,
            live_refresh_pending: false,
            live_analysis_dirty: true,

            selected_player,

            teams: vec![FantasyTeam {
                id: TeamId(0),
                name: String::from("DTV"),
                budget: 200,
                is_user: true,
            }],
            selected_roster_team: Some(0),
            user_team_id: TeamId(0),

            draft_picks: Vec::new(),
            draft_price_input: String::new(),
            draft_mode: DraftMode::BrowsingPlayers,
            selected_team: None,

            search_query: String::new(),

            builds: Vec::new(),
            replacements: Vec::new(),
            selected_replacement: 0,

            pending_edit_command: PendingEditCommand::None,

            player_register: None,
            player_form: None,
            data_dirty: false,

            team_input: None,
            teams_dirty: false,

            edit_status: None,
        }
    }

    fn single_player_test_app() -> App {
        test_app(
            vec![Player {
                id: PlayerId("bird".to_string()),
                name: String::from("Bird"),
                position: String::from("SF"),
                projected_value: 200,
                short_name: Some(String::from("Bird")),
            }],
            Some(0),
        )
    }

    #[test]
    fn selecting_next_moves_to_next_player() {
        let mut app = test_app(
            vec![
                Player {
                    id: PlayerId("bird".to_string()),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId("luka".to_string()),
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
                    short_name: Some(String::from("Luka")),
                },
            ],
            Some(0),
        );

        app.select_next();

        assert_eq!(app.selected_player, Some(1));
    }

    #[test]
    fn selecting_previous_on_first_players_stays_on_first_player() {
        let mut app = test_app(
            vec![
                Player {
                    id: PlayerId("bird".to_string()),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId("luka".to_string()),
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
                    short_name: Some(String::from("Luka")),
                },
            ],
            Some(0),
        );

        app.select_previous();

        assert_eq!(app.selected_player, Some(0));
    }

    #[test]
    fn selecting_next_on_last_players_stays_on_last_player() {
        let mut app = test_app(
            vec![
                Player {
                    id: PlayerId("bird".to_string()),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId("luka".to_string()),
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
                    short_name: Some(String::from("Luka")),
                },
            ],
            Some(1),
        );

        app.select_next();

        assert_eq!(app.selected_player, Some(1));
    }

    #[test]
    fn empty_board_naviagtion() {
        let mut app = test_app(vec![], None);

        app.select_next();
        app.select_previous();

        assert_eq!(app.selected_player, None);
    }

    #[test]
    fn nonexistent_player_index() {
        let mut app = test_app(
            vec![
                Player {
                    id: PlayerId("bird".to_string()),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId("luka".to_string()),
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
                    short_name: Some(String::from("Luka")),
                },
            ],
            None,
        );

        let invalid_player_index = app.players.len();
        let result = app.record_draft(invalid_player_index, 0, 10);

        assert_eq!(result, Err(DraftError::InvalidPlayer));
    }

    #[test]
    fn nonexistent_team_index() {
        let mut app = test_app(
            vec![
                Player {
                    id: PlayerId("bird".to_string()),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId("luka".to_string()),
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
                    short_name: Some(String::from("Luka")),
                },
            ],
            Some(0),
        );

        let invalid_team_index = app.teams.len();
        let result = app.record_draft(0, invalid_team_index, 10);

        assert_eq!(result, Err(DraftError::InvalidTeam));
    }

    #[test]
    fn unaffordable_draft_return_insufficient_funds() {
        let mut app = test_app(
            vec![
                Player {
                    id: PlayerId("bird".to_string()),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId("luka".to_string()),
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
                    short_name: Some(String::from("Luka")),
                },
            ],
            Some(0),
        );

        let result = app.record_draft(0, 0, 201);

        assert_eq!(result, Err(DraftError::InsufficientFunds));
    }

    #[test]
    fn drafting_already_drafted_player_returns_player_already_drafted() {
        let mut app = test_app(
            vec![
                Player {
                    id: PlayerId("bird".to_string()),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId("luka".to_string()),
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
                    short_name: Some(String::from("Luka")),
                },
            ],
            Some(0),
        );

        app.draft_picks.push(DraftPick {
            player_id: PlayerId("bird".to_string()),
            team_id: TeamId(0),
            price: 1,
        });

        let result = app.record_draft(0, 0, 1);

        assert_eq!(result, Err(DraftError::PlayerAlreadyDrafted));
    }

    #[test]
    fn successful_draft_records_pick_and_reduces_budget() {
        let mut app = single_player_test_app();

        let result = app.record_draft(0, 0, 37);

        assert_eq!(result, Ok(()));
        assert_eq!(app.teams[0].budget, 163);
        assert_eq!(app.draft_picks.len(), 1);

        let pick = &app.draft_picks[0];

        assert_eq!(pick.player_id, PlayerId("bird".to_string()));
        assert_eq!(pick.team_id, TeamId(0));
        assert_eq!(pick.price, 37);
    }

    #[test]
    fn beginning_draft_of_selected_player_opens_team_chooser() {
        let mut app = single_player_test_app();
        app.session_phase = SessionPhase::LiveDraft;

        app.begin_drafting_selected_player();

        assert_eq!(app.draft_mode, DraftMode::RecordingDraft);
        assert_eq!(app.selected_team, Some(0));
    }

    #[test]
    fn confirming_recorded_draft_records_pick_and_resets_input() {
        let mut app = single_player_test_app();
        app.session_phase = SessionPhase::LiveDraft;

        app.begin_drafting_selected_player();
        app.draft_price_input = String::from("37");

        app.confirm_recorded_draft();

        assert_eq!(app.draft_picks.len(), 1);
        assert_eq!(app.teams[0].budget, 163);
        assert_eq!(app.draft_mode, DraftMode::BrowsingPlayers);
        assert_eq!(app.selected_team, None);
        assert!(app.draft_price_input.is_empty());
    }

    #[test]
    fn escaping_recorded_draft_cancels_without_recording_pick() {
        let mut app = single_player_test_app();
        app.session_phase = SessionPhase::LiveDraft;

        app.begin_drafting_selected_player();
        app.draft_price_input = String::from("37");

        app.escape_drafting_selected_player();

        assert_eq!(app.draft_mode, DraftMode::BrowsingPlayers);
        assert_eq!(app.selected_team, None);
        assert!(app.draft_price_input.is_empty());
        assert!(app.draft_picks.is_empty());
        assert_eq!(app.teams[0].budget, 200);
    }

    #[test]
    fn undo_last_pick_removes_pick_and_refunds_team() {
        let mut app = single_player_test_app();

        let original_budget = app.teams[0].budget;

        app.record_draft(0, 0, 25).unwrap();

        assert_eq!(app.draft_picks.len(), 1);
        assert_eq!(app.teams[0].budget, original_budget - 25);

        let undone = app.undo_last_pick();

        assert!(undone);
        assert!(app.draft_picks.is_empty());
        assert_eq!(app.teams[0].budget, original_budget);
        assert_eq!(app.selected_player, Some(0));
    }

    #[test]
    fn undo_with_no_picks_is_harmless() {
        let mut app = single_player_test_app();

        let budgets: Vec<u8> = app.teams.iter().map(|team| team.budget).collect();

        let undone = app.undo_last_pick();

        assert!(!undone);
        assert!(app.draft_picks.is_empty());

        let budgets_after: Vec<u8> = app.teams.iter().map(|team| team.budget).collect();

        assert_eq!(budgets_after, budgets);
    }
    #[test]
    fn preparation_mode_blocks_nomination() {
        let mut app = single_player_test_app();

        app.begin_drafting_selected_player();

        assert_eq!(app.draft_mode, DraftMode::BrowsingPlayers);
        assert_eq!(app.selected_team, None);
    }

    #[test]
    fn preparation_mode_disables_rosters_and_strategy_navigation() {
        let mut app = single_player_test_app();

        app.handle_key(KeyEvent::new(
            KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.screen, Screen::Draft);

        app.handle_key(KeyEvent::new(
            KeyCode::Char('s'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.screen, Screen::Draft);
    }

    #[test]
    fn live_mode_allows_rosters_and_strategy_navigation() {
        let mut app = single_player_test_app();
        app.session_phase = SessionPhase::LiveDraft;
        app.home_phase_selection = SessionPhase::LiveDraft;

        app.handle_key(KeyEvent::new(
            KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.screen, Screen::Rosters);

        app.screen = Screen::Draft;
        app.handle_key(KeyEvent::new(
            KeyCode::Char('s'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.screen, Screen::Strategy);
    }

    #[test]
    fn home_selection_changes_session_mode_immediately() {
        let mut app = single_player_test_app();
        app.screen = Screen::Home;

        app.handle_key(KeyEvent::new(
            KeyCode::Char('j'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.home_phase_selection, SessionPhase::LiveDraft);
        assert_eq!(app.session_phase, SessionPhase::LiveDraft);
        assert!(app.live_refresh_pending);

        app.handle_key(KeyEvent::new(
            KeyCode::Char('k'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.home_phase_selection, SessionPhase::Preparation);
        assert_eq!(app.session_phase, SessionPhase::Preparation);
        assert!(!app.live_refresh_pending);
    }

    #[test]
    fn canonicalizes_and_deduplicates_legacy_player_identity() {
        let stats = StatsBundle {
            draft_season: String::from("2026-27"),
            source_season: String::from("2025-26"),
            cache_dir: PathBuf::new(),
            players: vec![stat_player("tatumja01", "Jayson Tatum")],
            weekly: Vec::new(),
        };

        let mut players = vec![
            Player {
                id: PlayerId("6".to_string()),
                name: String::from("Jayson Tatum"),
                short_name: Some(String::from("Tatum")),
                position: String::from("SF/PF"),
                projected_value: 50,
            },
            Player {
                id: PlayerId("tatumja01".to_string()),
                name: String::from("Jayson Tatum"),
                short_name: None,
                position: String::from("—"),
                projected_value: 0,
            },
        ];

        let aliases = canonicalize_curated_player_ids(&mut players, &stats);

        assert_eq!(players.len(), 1);
        assert_eq!(players[0].id, PlayerId("tatumja01".to_string()));
        assert_eq!(players[0].display_name(), "Tatum");
        assert_eq!(
            aliases.get(&PlayerId("6".to_string())),
            Some(&PlayerId("tatumja01".to_string()))
        );
    }

    #[test]
    fn preparation_live_switch_reuses_clean_live_analysis() {
        let mut app = single_player_test_app();
        app.live_analysis_dirty = false;
        app.live_advantage_board = vec![dummy_live_score("bird", "Bird")];

        app.select_home_phase(SessionPhase::Preparation);
        assert!(!app.live_refresh_pending);

        app.select_home_phase(SessionPhase::LiveDraft);
        assert!(!app.live_refresh_pending);
        assert!(!app.live_analysis_dirty);
        assert_eq!(app.live_advantage_board.len(), 1);
    }
}
