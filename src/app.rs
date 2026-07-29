use crate::data::load_players;
use crate::player::Player;
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
}

impl App {
    pub fn new() -> Result<App, csv::Error> {
        let players = load_players("data/players.csv")?;
        let selected_player = if players.is_empty() { None } else { Some(0) };
        Ok(App {
            running: true,
            screen: Screen::Home,
            players,
            selected_player,
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
}
