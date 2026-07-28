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
}

impl App {
    pub fn new() -> Result<App, csv::Error> {
        let players = load_players("data/players.csv")?;
        Ok(App {
            running: true,
            screen: Screen::Home,
            players,
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
            _ => {}
        }
    }
}
