use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, InputMode};

pub fn draw(frame: &mut Frame, app: &App) {
    let area: Rect = frame.area();

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(rows[0]);

    draw_roster(frame, app, columns[0]);
    draw_pane(frame, app, columns[1]);
    draw_status_line(frame, app, rows[1]);
}

fn draw_roster(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|session| ListItem::new(session.display_label()))
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Team"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    if !app.sessions.is_empty() {
        state.select(Some(app.selected));
    }

    frame.render_stateful_widget(list, area, &mut state);
}

/// Renders the selected session's most recently polled tmux pane text as raw,
/// unstyled text. This is a plain passthrough — it does NOT interpret ANSI/VT100
/// color or cursor-positioning escape sequences from the underlying terminal
/// program. Full VT100 rendering (e.g. via the `vt100` crate already in this
/// crate's dependencies) is an explicit, noted follow-up, not silently dropped.
fn draw_pane(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.selected_session() {
        Some(session) => format!("Pane: {}", session.display_label()),
        None => "Pane".to_string(),
    };
    let paragraph = Paragraph::new(app.pane_content.as_str())
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, area);
}

fn draw_status_line(frame: &mut Frame, app: &App, area: Rect) {
    let text = match app.input_mode {
        Some(InputMode::HarnessId) => {
            format!("hire> harness id: {}_  (Enter to continue, Esc to cancel)", app.input_harness_id)
        }
        Some(InputMode::Role) => format!(
            "hire> harness id: {}  role: {}_  (Enter to hire, Esc to cancel)",
            app.input_harness_id, app.input_role
        ),
        None => {
            "q: quit  up/down: select  h: hire  r: release  i: interrupt".to_string()
        }
    };
    frame.render_widget(Paragraph::new(text), area);
}
