use crate::app::App;
use crate::app::Screen;
use crate::views::draft;
use crate::views::home;
use crate::views::rosters;
use crate::views::strategy;

use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App) {
    match app.screen {
        Screen::Home => {
            home::draw(frame, app);
        }
        Screen::Draft => {
            draft::draw(frame, app);
        }
        Screen::Rosters => {
            rosters::draw(frame, app);
        }
        Screen::Strategy => {
            strategy::draw(frame, app);
        }
    }
}
