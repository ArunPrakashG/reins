use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use reins_core::SessionStatus;

use crate::app::{App, InputMode};
use crate::effects::{self, AnimationState};

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

/// Roster status glyph, matching the spec's §9 mockup: a filled dot for a team member
/// who is still on the job, a hollow one for a released or exited member.
fn status_glyph(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Starting | SessionStatus::Running | SessionStatus::AwaitingInput => "●",
        SessionStatus::Exited | SessionStatus::Killed => "○",
    }
}

fn draw_roster(frame: &mut Frame, app: &App, area: Rect) {
    // Released/exited members stay in the roster (they're still part of the project's
    // history, and the daemon still returns them) but are visually demoted rather than
    // filtered out: filtering would desynchronise the list indices from `app.selected`,
    // which every action — release, interrupt, pane polling — keys off. A glyph plus
    // dimming is the smaller, safer distinction.
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|session| {
            let text = format!("{} {}", status_glyph(session.status), session.display_label());
            let item = ListItem::new(text);
            match session.status {
                SessionStatus::Exited | SessionStatus::Killed => {
                    item.style(Style::default().add_modifier(Modifier::DIM))
                }
                _ => item,
            }
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Team"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    if !app.sessions.is_empty() {
        state.select(Some(app.selected));
    }

    frame.render_stateful_widget(list, area, &mut state);
    animate_roster_glyphs(frame, app, area, &state);
}

/// Post-processes the just-rendered roster's status-glyph cells with a looping tachyonfx
/// pulse for `Starting`/`Running` rows (Task 12), a no-op for everything else — including
/// the whole roster when `animations = false`, matching the MVP's static `●`/`○` glyphs.
///
/// Runs after the `List` widget has drawn into the frame's buffer rather than trying to
/// fold the effect into `ListItem` construction, since it needs the widget's own
/// scroll offset (`state.offset()`, only known once rendering has happened) to map a
/// session index back to the screen row its glyph landed on.
fn animate_roster_glyphs(frame: &mut Frame, app: &App, area: Rect, state: &ListState) {
    if !app.animations_enabled || area.width < 3 || area.height < 3 {
        return;
    }

    // Inside the `Team` block's border, matching `Block::default().borders(Borders::ALL)`.
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let offset = state.offset();
    let running_elapsed = app.started_at.elapsed();
    let buffer = frame.buffer_mut();

    for (index, session) in app.sessions.iter().enumerate().skip(offset) {
        let row = (index - offset) as u16;
        if row >= inner.height {
            break;
        }
        let anim_state = effects::animation_state_for(session.status, true);
        let elapsed = match anim_state {
            AnimationState::Static => continue,
            AnimationState::HiringPulse => app
                .hire_started_at
                .get(&session.id)
                .map(|instant| instant.elapsed())
                .unwrap_or_default(),
            AnimationState::RunningPulse => running_elapsed,
        };
        let glyph_area = Rect { x: inner.x, y: inner.y + row, width: 1, height: 1 };
        effects::apply_glyph_animation(buffer, glyph_area, anim_state, elapsed);
    }
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
            "hire> harness id: {}  role: {}_  (Enter to continue, Esc to cancel)",
            app.input_harness_id, app.input_role
        ),
        Some(InputMode::Brief) => format!(
            "hire> harness id: {}  role: {}  brief (optional): {}_  (Enter to hire, Esc to cancel)",
            app.input_harness_id, app.input_role, app.input_brief
        ),
        // In-loop errors surface here rather than on stderr, which would corrupt the
        // raw-mode frame.
        None => match &app.status_message {
            Some(message) => format!("{message}  (any key to dismiss)"),
            None => "q: quit  up/down: select  h: hire  r: release  i: interrupt".to_string(),
        },
    };
    frame.render_widget(Paragraph::new(text), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_glyph_distinguishes_live_from_finished_members() {
        assert_eq!(status_glyph(SessionStatus::Starting), "●");
        assert_eq!(status_glyph(SessionStatus::Running), "●");
        assert_eq!(status_glyph(SessionStatus::AwaitingInput), "●");
        assert_eq!(status_glyph(SessionStatus::Exited), "○");
        assert_eq!(status_glyph(SessionStatus::Killed), "○");
    }
}
