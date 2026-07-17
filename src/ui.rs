use crate::app::App;
use crate::app::Screen;
use crate::views::draft;
use crate::views::home;

use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App) {
    match app.screen {
        Screen::Home => {
            home::draw(frame);
        }
        Screen::Draft => {
            draft::draw(frame);
        }
    }
}
