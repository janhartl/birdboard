use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;

pub enum Screen {
    Home,
    Draft,
}

#[derive(Debug, Clone)]
pub struct Player {
    pub name: String,
    pub position: String,
    pub projected_value: u8,
}

pub struct App {
    pub running: bool,
    pub screen: Screen,
    pub players: Vec<Player>,
}

impl App {
    pub fn new() -> App {
        let players = vec![
            Player {
                name: String::from("Larry"),
                position: String::from("SF"),
                projected_value: 200,
            },
            Player {
                name: String::from("Luka"),
                position: String::from("PG"),
                projected_value: 77,
            },
        ];
        App {
            running: true,
            screen: Screen::Home,
            players: players,
        }
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
