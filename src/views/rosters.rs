use crate::app::App;
use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::widgets::Paragraph;

pub fn draw(frame: &mut Frame, _app: &App) {
    let placeholder = Paragraph::new("Rosters").alignment(Alignment::Center);

    frame.render_widget(placeholder, frame.area());
}
