use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::app::App;

pub fn draw(frame: &mut Frame, app: &App) {
    let area: Rect = frame.area();

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
