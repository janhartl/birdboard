use crate::data::load_players;
use crate::data::load_teams;
use crate::data::save_players;
use crate::data::validate_data;
use crate::draft::DraftError;
use crate::draft::DraftMode;
use crate::draft::DraftPick;
use crate::player::{Player, PlayerId};
use crate::strategy::{
    Build, ReplacementGroup, active_build, load_builds, load_replacements, replacement_for_player,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Preparation,
    LiveDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardMode {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

pub struct App {
    pub running: bool,
    pub screen: Screen,
    pub session_phase: SessionPhase,
    pub board_mode: BoardMode,
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
    pub replacements: Vec<ReplacementGroup>,
    pub selected_replacement: usize,
    pub pending_edit_command: PendingEditCommand,
    pub player_register: Option<PlayerRegister>,
    pub data_dirty: bool,
    pub edit_status: Option<String>,
    pub player_form: Option<PlayerForm>,
}

impl App {
    pub fn new() -> Result<App> {
        let players = load_players("data/players.csv")?;
        let selected_player = if players.is_empty() { None } else { Some(0) };
        let teams = load_teams("data/teams.csv")?;
        let draft_picks = Vec::new();
        let builds = load_builds("data/builds.toml")?;
        let replacements = load_replacements("data/replacements.toml")?;

        validate_data(&players, &teams, &builds, &replacements)?;

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
            replacements,
            selected_replacement: 0,
            session_phase: SessionPhase::Preparation,
            board_mode: BoardMode::Browse,
            pending_edit_command: PendingEditCommand::None,
            player_register: None,
            data_dirty: false,
            edit_status: None,
            player_form: None,
        })
    }

    pub fn quit(&mut self) {
        self.running = false;
    }
    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.player_form.is_some() {
            self.handle_player_form_key(key);
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
            && matches!(&self.board_mode, BoardMode::Edit)
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

                        Err(error) => {
                            format!("SAVE FAILED: {error}")
                        }
                    };

                    self.edit_status = Some(status);
                }

                KeyCode::Char('E') | KeyCode::Esc => {
                    self.pending_edit_command = PendingEditCommand::None;

                    // Do not leave edit mode while a player is cut.
                    if self.player_register.is_none() {
                        self.board_mode = BoardMode::Browse;
                    }
                }

                _ => {
                    // An unrelated key cancels a pending first `d`.
                    self.pending_edit_command = PendingEditCommand::None;
                }
            }

            return;
        }
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
            KeyCode::Char('E')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.toggle_board_edit_mode();
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
            KeyCode::Char('j') if matches!(&self.screen, Screen::Strategy) => {
                self.select_next_replacement();
            }

            KeyCode::Char('k') if matches!(&self.screen, Screen::Strategy) => {
                self.select_previous_replacement();
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

            KeyCode::Enter
                if matches!(&self.screen, Screen::Draft)
                    && matches!(self.draft_mode, DraftMode::BrowsingPlayers)
                    && matches!(&self.board_mode, BoardMode::Browse)
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
            KeyCode::Char('/')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.search_query.clear();
                self.draft_mode = DraftMode::SearchingPlayer;
            }
            KeyCode::Char('u')
                if matches!(&self.screen, Screen::Draft)
                    && matches!(&self.draft_mode, DraftMode::BrowsingPlayers) =>
            {
                self.undo_last_pick();
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
        self.session_phase = SessionPhase::LiveDraft;
        self.board_mode = BoardMode::Browse;

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
            .copied()
            .filter(|player_id| {
                matches!(
                    self.draft_pick_for_player(*player_id),
                    Some(pick)
                        if pick.team_id != self.user_team_id
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

        true
    }
    pub fn editing_allowed(&self) -> bool {
        matches!(self.session_phase, SessionPhase::Preparation)
    }
    pub fn toggle_board_edit_mode(&mut self) {
        if !self.editing_allowed() {
            return;
        }
        self.board_mode = match self.board_mode {
            BoardMode::Browse => BoardMode::Edit,
            BoardMode::Edit => BoardMode::Browse,
        };
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

        save_players("data/players.csv", &self.players)?;

        self.data_dirty = false;

        // A successful save while a player is still cut means
        // that the deletion has been confirmed.
        self.player_register = None;

        Ok(())
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
            mode: PlayerFormMode::Edit(player.id),
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

        let mode = form.mode;
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
                let Some(player_id) = self.next_available_player_id() else {
                    self.set_player_form_error("No unused player IDs remain.");
                    return;
                };

                let insert_index = match self.selected_player {
                    Some(index) => (index + 1).min(self.players.len()),

                    None => 0,
                };

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

    fn next_available_player_id(&self) -> Option<PlayerId> {
        let largest_id = self
            .players
            .iter()
            .map(|player| player.id.0)
            .chain(
                self.player_register
                    .iter()
                    .map(|register| register.player.id.0),
            )
            .max();

        match largest_id {
            Some(id) => id.checked_add(1).map(PlayerId),

            None => Some(PlayerId(0)),
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
            replacements: Vec::new(),
            selected_replacement: 0,
            session_phase: SessionPhase::Preparation,
            board_mode: BoardMode::Browse,
            pending_edit_command: PendingEditCommand::None,
            player_register: None,
            data_dirty: false,
            edit_status: None,
            player_form: None,
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
    #[test]
    fn undo_last_pick_removes_pick_and_refunds_team() {
        let mut app = test_app();

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
        let mut app = test_app();

        let budgets: Vec<u8> = app.teams.iter().map(|team| team.budget).collect();

        let undone = app.undo_last_pick();

        assert!(!undone);
        assert!(app.draft_picks.is_empty());

        let budgets_after: Vec<u8> = app.teams.iter().map(|team| team.budget).collect();

        assert_eq!(budgets_after, budgets);
    }
}
