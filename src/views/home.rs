use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::widgets::Paragraph;

const LOGO: &str = r#"
          \
      \\ \
   \\\\ \
\\\\\\ \
      
B I R D B O A R D
Who's playing for 2nd?

      "#;

pub fn draw(frame: &mut Frame) {
    let logo = Paragraph::new(LOGO).alignment(Alignment::Center);
    let placeholder = Paragraph::new("Big board incomming...").alignment(Alignment::Center);
    let quiting = Paragraph::new("Press 'q' to quit").alignment(Alignment::Left);

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());
    frame.render_widget(logo, areas[0]);
    frame.render_widget(placeholder, areas[1]);
    frame.render_widget(quiting, areas[2]);
}
