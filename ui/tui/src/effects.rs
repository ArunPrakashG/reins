//! Branded ASCII-wordmark splash, played once on every `reins` launch (Task 11).
//!
//! Uses `tachyonfx` 0.7 (pinned to `ratatui = "0.28.1"`, matching this workspace's own
//! `ratatui = "0.28"` pin exactly — every later tachyonfx release requires `ratatui`
//! 0.29+ and the post-split `ratatui-core` crate, which is incompatible with this
//! workspace) to animate the wordmark in with a `coalesce` entrance effect.

use crossterm::event::{self, Event};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Rect};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io::Stdout;
use std::time::{Duration as StdDuration, Instant};
use tachyonfx::{fx, Effect, EffectRenderer, EffectTimer, Interpolation, Shader};

use crate::config;

const WORDMARK: &str = r#"██████╗ ███████╗██╗███╗   ██╗███████╗
██╔══██╗██╔════╝██║████╗  ██║██╔════╝
██████╔╝█████╗  ██║██╔██╗ ██║███████╗
██╔══██╗██╔══╝  ██║██║╚██╗██║╚════██║
██║  ██║███████╗██║██║ ╚████║███████║
╚═╝  ╚═╝╚══════╝╚═╝╚═╝  ╚═══╝╚══════╝"#;

/// Total entrance-effect duration; comfortably inside the ~800ms-1.2s target from the
/// brief.
const SPLASH_DURATION_MS: u32 = 900;

/// How often the render loop wakes to check for a dismissing keypress. Short enough to
/// keep the animation smooth without pegging a CPU core for the ~1s the splash runs.
const POLL_INTERVAL: StdDuration = StdDuration::from_millis(16);

/// Plays the "REINS" wordmark splash with a tachyonfx entrance effect. Renders nothing
/// and returns immediately, without entering the render loop at all, if animations are
/// disabled via config (Task 6) or the terminal reports zero size. Also exits early on
/// the first keypress so the splash never blocks an impatient user, and unconditionally
/// once the effect reports complete.
pub fn play_splash(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    if !config::load().animations {
        return Ok(());
    }

    let lines: Vec<&str> = WORDMARK.lines().collect();
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let height = lines.len() as u16;
    if width == 0 || height == 0 {
        return Ok(());
    }

    let term_size = terminal.size()?;
    if term_size.width == 0 || term_size.height == 0 {
        return Ok(());
    }

    let area = Rect::new(
        term_size.width.saturating_sub(width) / 2,
        term_size.height.saturating_sub(height) / 2,
        width.min(term_size.width),
        height.min(term_size.height),
    );

    let timer = EffectTimer::from_ms(SPLASH_DURATION_MS, Interpolation::QuadOut);
    let mut effect: Effect = fx::coalesce(timer);

    let mut last = Instant::now();
    loop {
        let now = Instant::now();
        let elapsed: tachyonfx::Duration = (now - last).into();
        last = now;

        terminal.draw(|frame| {
            let paragraph = Paragraph::new(WORDMARK).alignment(Alignment::Center);
            frame.render_widget(paragraph, area);
            frame.render_effect(&mut effect, area, elapsed);
        })?;

        if effect.done() {
            break;
        }

        if event::poll(POLL_INTERVAL)? {
            if let Event::Key(_) = event::read()? {
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordmark_is_non_empty_and_rectangular() {
        let lines: Vec<&str> = WORDMARK.lines().collect();
        assert!(!lines.is_empty());
        let width = lines[0].chars().count();
        assert!(lines.iter().all(|l| l.chars().count() == width));
    }
}
