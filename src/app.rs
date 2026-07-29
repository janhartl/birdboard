use crate::data::load_players;
use crate::draft::DraftError;
use crate::draft::DraftPick;
use crate::player::Player;
use crate::team::FantasyTeam;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;

pub enum Screen {
    Home,
    Draft,
}

pub struct App {
    pub running: bool,
    pub screen: Screen,
    pub players: Vec<Player>,
    pub selected_player: Option<usize>,
    pub teams: Vec<FantasyTeam>,
    pub draft_picks: Vec<DraftPick>,
}

impl App {
    pub fn new() -> Result<App, csv::Error> {
        let players = load_players("data/players.csv")?;
        let selected_player = if players.is_empty() { None } else { Some(0) };
        let teams = vec![
            FantasyTeam {
                name: String::from("Luka Legends"),
                budget: 200,
            },
            FantasyTeam {
                name: String::from("Drustvo telesne vadbe"),
                budget: 200,
            },
        ];
        let draft_picks = Vec::new();
        Ok(App {
            running: true,
            screen: Screen::Home,
            players,
            selected_player,
            teams,
            draft_picks,
        })
    }

    pub fn quit(&mut self) {
        self.running = false;
    }
    pub fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit(),
            KeyCode::Char('1') => self.screen = Screen::Home,
            KeyCode::Char('2') => self.screen = Screen::Draft,

            KeyCode::Char('j') if matches!(&self.screen, Screen::Draft) => self.select_next(),
            KeyCode::Char('k') if matches!(&self.screen, Screen::Draft) => self.select_previous(),
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

        let player_already_drafted = self
            .draft_picks
            .iter()
            .any(|pick| pick.player_index == player_index);

        if player_already_drafted {
            return Err(DraftError::PlayerAlreadyDrafted);
        }
        let team = &self.teams[team_index];

        if price > team.budget {
            return Err(DraftError::InsufficientFunds);
        }

        self.teams[team_index].budget -= price;
        let draft_pick = DraftPick {
            player_index,
            team_index,
            price,
        };
        self.draft_picks.push(draft_pick);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(players: Vec<Player>, selected_player: Option<usize>) -> App {
        App {
            running: true,
            screen: Screen::Draft,
            players,
            selected_player,
            teams: vec![FantasyTeam {
                name: String::from("DTV"),
                budget: 200,
            }],
            draft_picks: Vec::new(),
        }
    }

    #[test]
    fn selecting_next_moves_to_next_player() {
        let mut app = test_app(
            vec![
                Player {
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                },
                Player {
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
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
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                },
                Player {
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
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
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                },
                Player {
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
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
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                },
                Player {
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
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
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                },
                Player {
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
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
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                },
                Player {
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
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
                    name: String::from("Bird"),
                    position: String::from("SF"),
                    projected_value: 200,
                },
                Player {
                    name: String::from("Luka"),
                    position: String::from("PG"),
                    projected_value: 77,
                },
            ],
            Some(0),
        );
        app.draft_picks.push(DraftPick {
            player_index: 0,
            team_index: 0,
            price: 1,
        });
        let result = app.record_draft(0, 0, 1);
        assert_eq!(result, Err(DraftError::PlayerAlreadyDrafted));
    }
    #[test]
    fn successful_draft_records_pick_and_reduces_budget() {
        let mut app = test_app(
            vec![Player {
                name: String::from("Bird"),
                position: String::from("SF"),
                projected_value: 200,
            }],
            Some(0),
        );

        let result = app.record_draft(0, 0, 37);

        assert_eq!(result, Ok(()));
        assert_eq!(app.teams[0].budget, 163);
        assert_eq!(app.draft_picks.len(), 1);

        let pick = &app.draft_picks[0];
        assert_eq!(pick.player_index, 0);
        assert_eq!(pick.team_index, 0);
        assert_eq!(pick.price, 37);
    }
}
