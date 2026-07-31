use crate::data::load_players;
use crate::data::load_teams;
use crate::draft::DraftError;
use crate::draft::DraftMode;
use crate::draft::DraftPick;
use crate::player::{Player, PlayerId};
use crate::strategy::{Build, load_builds};
use crate::team::FantasyTeam;
use crate::team::TeamId;
use anyhow::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

pub enum Screen {
    Home,
    Draft,
    Rosters,
    Strategy,
}

pub struct App {
    pub running: bool,
    pub screen: Screen,
    pub players: Vec<Player>,
    pub selected_player: Option<usize>,
    pub teams: Vec<FantasyTeam>,
    pub draft_picks: Vec<DraftPick>,
    pub draft_price_input: String,
    pub draft_mode: DraftMode,
    pub selected_team: Option<usize>,
    pub search_query: String,
    pub user_team_id: TeamId,
    pub builds: Vec<Build>,
}

impl App {
    pub fn new() -> Result<App> {
        let players = load_players("data/players.csv")?;
        let selected_player = if players.is_empty() { None } else { Some(0) };
        let teams = load_teams("data/teams.csv")?;
        let draft_picks = Vec::new();
        let builds = load_builds("data/builds.toml")?;
        Ok(App {
            running: true,
            screen: Screen::Home,
            players,
            selected_player,
            teams,
            draft_picks,
            draft_mode: DraftMode::BrowsingPlayers,
            selected_team: None,
            draft_price_input: String::new(),
            search_query: String::new(),
            user_team_id: TeamId(1),
            builds,
        })
    }

    pub fn quit(&mut self) {
        self.running = false;
    }
    pub fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') if !matches!(self.draft_mode, DraftMode::SearchingPlayer) => {
                self.quit()
            }
            KeyCode::Char('h') if matches!(&self.draft_mode, DraftMode::BrowsingPlayers) => {
                self.screen = Screen::Home;
            }
            KeyCode::Char('b') if matches!(&self.draft_mode, DraftMode::BrowsingPlayers) => {
                self.screen = Screen::Draft;
            }
            KeyCode::Char('r') if matches!(&self.draft_mode, DraftMode::BrowsingPlayers) => {
                self.screen = Screen::Rosters;
            }
            KeyCode::Char('s') if matches!(&self.draft_mode, DraftMode::BrowsingPlayers) => {
                self.screen = Screen::Strategy;
            }

            KeyCode::Char('j')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.select_next()
            }
            KeyCode::Char('k')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.select_previous()
            }

            KeyCode::Char('j')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.select_next_team()
            }
            KeyCode::Char('k')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.select_previous_team()
            }
            KeyCode::Char(digit)
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft)
                    && digit.is_ascii_digit()
                    && self.draft_price_input.len() < 3 =>
            {
                self.draft_price_input.push(digit);
            }
            KeyCode::Backspace
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.draft_price_input.pop();
            }
            KeyCode::Esc
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.escape_drafting_selected_player();
            }
            KeyCode::Esc
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::SearchingPlayer) =>
            {
                self.search_query.clear();
                self.draft_mode = DraftMode::BrowsingPlayers;
            }
            KeyCode::Enter
                if matches!(&self.screen, Screen::Draft)
                    && matches!(self.draft_mode, DraftMode::BrowsingPlayers)
                    && let Some(player_index) = self.selected_player
                    && self
                        .draft_pick_for_player(self.players[player_index].id)
                        .is_none() =>
            {
                self.begin_drafting_selected_player();
            }
            KeyCode::Enter
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::RecordingDraft) =>
            {
                self.confirm_recorded_draft();
            }
            KeyCode::Enter
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::SearchingPlayer) =>
            {
                self.search_query.clear();
                self.draft_mode = DraftMode::BrowsingPlayers;
            }
            KeyCode::Backspace
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::SearchingPlayer) =>
            {
                self.search_query.pop();
                self.update_search_selection();
            }
            KeyCode::Char('/')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.search_query.clear();
                self.draft_mode = DraftMode::SearchingPlayer;
            }
            KeyCode::Char(char)
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::SearchingPlayer) =>
            {
                self.search_query.push(char);
                self.update_search_selection();
            }
            _ => {}
        }
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
        let player_id = self.players[player_index].id;
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
        let draft_pick = DraftPick {
            player_id,
            team_id,
            price,
        };
        self.draft_picks.push(draft_pick);

        Ok(())
    }
    pub fn begin_drafting_selected_player(&mut self) {
        if self.selected_player.is_some() && !self.teams.is_empty() {
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
    }
    pub fn draft_pick_for_player(&self, player_id: PlayerId) -> Option<&DraftPick> {
        self.draft_picks
            .iter()
            .find(|pick| pick.player_id == player_id)
    }
    pub fn player_by_id(&self, player_id: PlayerId) -> Option<&Player> {
        self.players.iter().find(|player| player.id == player_id)
    }
    pub fn team_has_player(&self, team_id: TeamId, player_id: PlayerId) -> bool {
        self.draft_picks
            .iter()
            .any(|pick| pick.team_id == team_id && pick.player_id == player_id)
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

#[cfg(test)]
mod tests {
    use crate::player::PlayerId;
    use crate::team::TeamId;

    use super::*;

    fn test_app(players: Vec<Player>, selected_player: Option<usize>) -> App {
        App {
            running: true,
            screen: Screen::Draft,
            players,
            selected_player,
            teams: vec![FantasyTeam {
                id: TeamId(0),
                name: String::from("DTV"),
                budget: 200,
            }],
            draft_picks: Vec::new(),
            draft_mode: DraftMode::BrowsingPlayers,
            selected_team: None,
            draft_price_input: String::new(),
            search_query: String::new(),
            user_team_id: TeamId(1),
            builds: Vec::new(),
        }
    }

    #[test]
    fn selecting_next_moves_to_next_player() {
        let mut app = test_app(
            vec![
                Player {
                    id: PlayerId(0),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId(1),
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
                    id: PlayerId(0),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId(1),
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
                    id: PlayerId(0),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId(1),
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
                    id: PlayerId(0),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId(1),
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
                    id: PlayerId(0),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId(1),
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
                    id: PlayerId(0),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId(1),
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
                    id: PlayerId(0),
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                    short_name: Some(String::from("Bird")),
                },
                Player {
                    id: PlayerId(1),
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
                    short_name: Some(String::from("Luka")),
                },
            ],
            Some(0),
        );
        app.draft_picks.push(DraftPick {
            player_id: PlayerId(0),
            team_id: TeamId(0),
            price: 1,
        });
        let result = app.record_draft(0, 0, 1);
        assert_eq!(result, Err(DraftError::PlayerAlreadyDrafted));
    }
    #[test]
    fn successful_draft_records_pick_and_reduces_budget() {
        let mut app = test_app(
            vec![Player {
                id: PlayerId(0),
                name: String::from("Bird"),
                position: String::from("SF"),
                projected_value: 200,
                short_name: Some(String::from("Bird")),
            }],
            Some(0),
        );

        let result = app.record_draft(0, 0, 37);

        assert_eq!(result, Ok(()));
        assert_eq!(app.teams[0].budget, 163);
        assert_eq!(app.draft_picks.len(), 1);

        let pick = &app.draft_picks[0];
        assert_eq!(pick.player_id, PlayerId(0));
        assert_eq!(pick.team_id, TeamId(0));
        assert_eq!(pick.price, 37);
    }
    #[test]
    fn beginning_draft_of_selected_player_opens_team_chooser() {
        let mut app = test_app(
            vec![Player {
                id: PlayerId(0),
                name: String::from("Bird"),
                position: String::from("SF"),
                projected_value: 50,
                short_name: Some(String::from("Bird")),
            }],
            Some(0),
        );

        app.begin_drafting_selected_player();

        assert_eq!(app.draft_mode, DraftMode::RecordingDraft);
        assert_eq!(app.selected_team, Some(0));
    }
    #[test]
    fn confirming_recorded_draft_records_pick_and_resets_input() {
        let mut app = test_app(
            vec![Player {
                id: PlayerId(0),
                name: String::from("Bird"),
                position: String::from("SF"),
                projected_value: 50,
                short_name: Some(String::from("Bird")),
            }],
            Some(0),
        );

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
        let mut app = test_app(
            vec![Player {
                id: PlayerId(0),
                name: String::from("Bird"),
                position: String::from("SF"),
                projected_value: 50,
                short_name: Some(String::from("Bird")),
            }],
            Some(0),
        );

        app.begin_drafting_selected_player();
        app.draft_price_input = String::from("37");

        app.escape_drafting_selected_player();

        assert_eq!(app.draft_mode, DraftMode::BrowsingPlayers);
        assert_eq!(app.selected_team, None);
        assert!(app.draft_price_input.is_empty());

        assert!(app.draft_picks.is_empty());
        assert_eq!(app.teams[0].budget, 200);
    }
}
