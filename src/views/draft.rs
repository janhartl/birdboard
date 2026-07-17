use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::widgets::Paragraph;

pub fn draw(frame: &mut Frame) {
    let placeholder = Paragraph::new("Larry Bird the GOAT").alignment(Alignment::Center);
    let quiting = Paragraph::new("Press 'q' to quit").alignment(Alignment::Left);

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());
    frame.render_widget(placeholder, areas[0]);
    frame.render_widget(quiting, areas[1]);
}
