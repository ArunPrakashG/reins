use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
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

    // Drawn last so it overlays the roster/pane rather than being covered by them.
    if app.input_mode.is_some() {
        draw_hire_dialog(frame, app);
    }
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

/// Post-processes the just-rendered roster's status-glyph cells with a looping pulse
/// for `Starting`/`Running` rows (Task 12), a no-op for everything else — including the
/// whole roster when `animations = false`, matching the MVP's static `●`/`○` glyphs.
///
/// Runs after the `List` widget has drawn into the frame's buffer rather than trying to
/// fold the effect into `ListItem` construction, since it needs the widget's own
/// scroll offset (`state.offset()`, only known once rendering has happened) to map a
/// session index back to the screen row its glyph landed on.
///
/// Each row's pulse color is computed directly by `effects::apply_glyph_animation` as a
/// pure function of elapsed time (hire time for a `Starting` row, app start for a
/// `Running` row) — see that function's doc comment for why this doesn't drive a
/// `tachyonfx::Effect` the way `effects::play_splash` does for the one-shot splash: two
/// independent bugs were found in review when a `tachyonfx` `Effect`-based approach was
/// tried for this looping breathing pulse.
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

/// Renders the selected session's most recently polled pane content with real color
/// and style — `app.pane_content` is tmux's `capture-pane -e` output (SGR escape codes
/// intact), parsed here with `vt100` into a screen grid and rendered cell-by-cell, so a
/// harness's own spinners/colors/box-drawing show up correctly instead of a flat text
/// dump. When focused, also draws the pane's real cursor (from `app.pane_cursor`).
fn draw_pane(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.selected_session() {
        Some(session) => {
            let focus_hint = if app.is_focused() { " [FOCUSED — Ctrl-B d to release]" } else { "" };
            format!("Pane: {}{focus_hint}", session.display_label())
        }
        None => "Pane".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let mut parser = vt100::Parser::new(inner.height, inner.width, 0);
    parser.process(app.pane_content.as_bytes());
    let screen = parser.screen();

    let mut lines = Vec::with_capacity(inner.height as usize);
    for row in 0..inner.height {
        let mut spans = Vec::with_capacity(inner.width as usize);
        for col in 0..inner.width {
            let Some(cell) = screen.cell(row, col) else { continue };
            // The second half of a wide (e.g. CJK) character is its own cell in vt100's
            // grid but carries no text of its own — the preceding cell already emitted
            // both display columns' worth of content.
            if cell.is_wide_continuation() {
                continue;
            }
            let text = if cell.has_contents() { cell.contents() } else { " ".to_string() };
            spans.push(Span::styled(text, cell_style(cell)));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), inner);

    if app.is_focused() {
        let (cursor_x, cursor_y) = app.pane_cursor;
        if cursor_x < inner.width && cursor_y < inner.height {
            frame.set_cursor_position((inner.x + cursor_x, inner.y + cursor_y));
        }
    }
}

/// Maps one `vt100` cell's color/style attributes onto a ratatui `Style`.
fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default().fg(map_vt100_color(cell.fgcolor())).bg(map_vt100_color(cell.bgcolor()));
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn map_vt100_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn draw_status_line(frame: &mut Frame, app: &App, area: Rect) {
    // Priority, highest first: focus mode owns the whole status line (every other key
    // is going to the pane, so the legend needs to say so); an open hire dialog owns
    // the input hint next (its own detail lives in the dialog itself, drawn
    // separately); a live quit-confirmation warning is the most urgent thing to show
    // once neither of those is active; then a one-shot status message; then the
    // default key legend.
    let text = if app.is_focused() {
        "FOCUSED — typing goes to the pane. Ctrl-B d: back to roster".to_string()
    } else if app.input_mode.is_some() {
        "Esc: cancel".to_string()
    } else if app.quit_warning_active() {
        "press q or Ctrl+C again to quit".to_string()
    } else {
        match &app.status_message {
            // In-loop errors surface here rather than on stderr, which would corrupt
            // the raw-mode frame.
            Some(message) => format!("{message}  (any key to dismiss)"),
            None => {
                "q: quit  up/down: select  Enter: focus pane  h: hire  r: release  i: interrupt"
                    .to_string()
            }
        }
    };
    frame.render_widget(Paragraph::new(text), area);
}

/// Renders the three-step hire prompt as a centered modal dialog over the roster/pane
/// area, replacing the MVP's single-line bottom-of-screen prompt. Keyboard-only, same
/// as the rest of the app: arrow keys move the harness picker, Enter advances/confirms,
/// Esc cancels — no mouse interaction of any kind.
fn draw_hire_dialog(frame: &mut Frame, app: &App) {
    let Some(mode) = app.input_mode else { return };

    let area = centered_rect(60, 50, frame.area());
    // Clears whatever the roster/pane already drew in this region — otherwise their
    // content would show through a widget that doesn't itself paint every cell.
    frame.render_widget(Clear, area);

    let lines: Vec<Line> = match mode {
        InputMode::HarnessId => {
            let mut lines = vec![Line::from("Select a harness to hire:"), Line::from("")];
            for (index, profile) in app.available_harnesses.iter().enumerate() {
                let highlighted = index == app.harness_picker_index;
                let marker = if highlighted { "> " } else { "  " };
                let style = if highlighted {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(
                    format!("{marker}{}", profile.display_name),
                    style,
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("up/down: choose   Enter: continue   Esc: cancel"));
            lines
        }
        InputMode::WorkingDirectory => vec![
            Line::from(format!("Harness: {}", app.input_harness_id)),
            Line::from(""),
            Line::from("Working directory (pre-filled with reins' own — edit or accept):"),
            Line::from(format!("{}_", app.input_working_dir)),
            Line::from(""),
            Line::from("Enter: continue   Esc: cancel"),
        ],
        InputMode::Role => vec![
            Line::from(format!("Harness: {}", app.input_harness_id)),
            Line::from(format!("Directory: {}", app.input_working_dir)),
            Line::from(""),
            Line::from(format!("Role: {}_", app.input_role)),
            Line::from(""),
            Line::from("Enter: continue   Esc: cancel"),
        ],
        InputMode::Brief => vec![
            Line::from(format!("Harness: {}", app.input_harness_id)),
            Line::from(format!("Directory: {}", app.input_working_dir)),
            Line::from(format!("Role: {}", app.input_role)),
            Line::from(""),
            Line::from(format!("Brief (optional): {}_", app.input_brief)),
            Line::from(""),
            Line::from("Enter: hire   Esc: cancel"),
        ],
    };

    let dialog = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Hire"));
    frame.render_widget(dialog, area);
}

/// A `Rect` of `percent_x`% width and `percent_y`% height, centered within `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
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
