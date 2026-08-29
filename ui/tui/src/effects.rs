//! Branded ASCII-wordmark splash, played once on every `reins` launch (Task 11).
//!
//! Uses `tachyonfx` 0.7 (pinned to `ratatui = "0.28.1"`, matching this workspace's own
//! `ratatui = "0.28"` pin exactly — every later tachyonfx release requires `ratatui`
//! 0.29+ and the post-split `ratatui-core` crate, which is incompatible with this
//! workspace) to animate the wordmark in with a `coalesce` entrance effect.

use crossterm::event::{self, Event};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Color;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io::Stdout;
use std::time::{Duration as StdDuration, Instant};
use tachyonfx::{fx, Effect, EffectRenderer, EffectTimer, Interpolation, Shader};

use crate::config;
use reins_core::SessionStatus;

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

/// Which animation treatment a roster row's status glyph should receive (Task 12).
///
/// This is deliberately kept as pure, tachyonfx-free decision logic (see
/// [`animation_state_for`]): the actual color-cycling effects are visual output that
/// can't be meaningfully unit tested, but *which* row gets *which* treatment is a plain
/// function of status and the `animations` config flag, and that's what's tested here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationState {
    /// No motion: either animations are disabled globally, or this status is inherently
    /// steady (`AwaitingInput` per spec §8, or a terminal `Exited`/`Killed` status).
    Static,
    /// Looping "hiring" pulse for a session still in `Starting`. Loops indefinitely
    /// (rather than playing once, like the splash) since hire duration is unbounded.
    HiringPulse,
    /// Subtle continuous pulse for a session in `Running`.
    RunningPulse,
}

/// Decides the animation treatment for one roster row. Pure function, no rendering.
pub fn animation_state_for(status: SessionStatus, animations_enabled: bool) -> AnimationState {
    if !animations_enabled {
        return AnimationState::Static;
    }
    match status {
        SessionStatus::Starting => AnimationState::HiringPulse,
        SessionStatus::Running => AnimationState::RunningPulse,
        SessionStatus::AwaitingInput | SessionStatus::Exited | SessionStatus::Killed => {
            AnimationState::Static
        }
    }
}

/// Period of one full hiring-pulse cycle (fade out and back).
const HIRING_PULSE_PERIOD_MS: u64 = 1200;

/// Period of one full running-pulse cycle — slower and subtler than the hiring pulse so
/// the two read as visually distinct at a glance.
const RUNNING_PULSE_PERIOD_MS: u64 = 2400;

fn hiring_pulse_effect() -> Effect {
    // A brighter, faster fade — reads as an attention-getting "still hiring" shimmer.
    let half = EffectTimer::from_ms((HIRING_PULSE_PERIOD_MS / 2) as u32, Interpolation::SineInOut);
    fx::ping_pong(fx::fade_to_fg(Color::Yellow, half))
}

fn running_pulse_effect() -> Effect {
    // A gentler, slower fade — present but not distracting for a long-lived row.
    let half =
        EffectTimer::from_ms((RUNNING_PULSE_PERIOD_MS / 2) as u32, Interpolation::SineInOut);
    fx::ping_pong(fx::fade_to_fg(Color::Green, half))
}

/// Applies the glyph animation for one roster row directly to `buffer` at `area` (which
/// should cover just the status-glyph cell, not the whole row).
///
/// There's no persistent per-row `Effect` object carried between frames — the roster is
/// a dynamically-sized, reorderable list, so there's nowhere natural to keep one. Instead
/// this builds a fresh looping effect each call and drives it directly to `elapsed`
/// (wall-clock time since the row's animation "began" — hire time for [`AnimationState::HiringPulse`],
/// app start for [`AnimationState::RunningPulse`]) reduced modulo the effect's own period,
/// which is equivalent to a continuously-looping effect since both fades are pure
/// functions of elapsed-within-cycle rather than of render-to-render deltas.
pub fn apply_glyph_animation(
    buffer: &mut Buffer,
    area: Rect,
    state: AnimationState,
    elapsed: StdDuration,
) {
    let (mut effect, period_ms) = match state {
        AnimationState::Static => return,
        AnimationState::HiringPulse => (hiring_pulse_effect(), HIRING_PULSE_PERIOD_MS),
        AnimationState::RunningPulse => (running_pulse_effect(), RUNNING_PULSE_PERIOD_MS),
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    let phase_ms = (elapsed.as_millis() as u64) % period_ms;
    let phase: tachyonfx::Duration = StdDuration::from_millis(phase_ms).into();
    let _ = effect.process(phase, buffer, area);
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

    #[test]
    fn everything_is_static_when_animations_are_disabled() {
        for status in [
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::AwaitingInput,
            SessionStatus::Exited,
            SessionStatus::Killed,
        ] {
            assert_eq!(animation_state_for(status, false), AnimationState::Static);
        }
    }

    #[test]
    fn starting_sessions_get_the_hiring_pulse_when_animated() {
        assert_eq!(
            animation_state_for(SessionStatus::Starting, true),
            AnimationState::HiringPulse
        );
    }

    #[test]
    fn running_sessions_get_the_running_pulse_when_animated() {
        assert_eq!(
            animation_state_for(SessionStatus::Running, true),
            AnimationState::RunningPulse
        );
    }

    #[test]
    fn awaiting_input_and_terminal_statuses_stay_static_even_when_animated() {
        assert_eq!(
            animation_state_for(SessionStatus::AwaitingInput, true),
            AnimationState::Static
        );
        assert_eq!(
            animation_state_for(SessionStatus::Exited, true),
            AnimationState::Static
        );
        assert_eq!(
            animation_state_for(SessionStatus::Killed, true),
            AnimationState::Static
        );
    }

    #[test]
    fn applying_a_static_animation_leaves_the_buffer_untouched() {
        let area = Rect::new(0, 0, 1, 1);
        let mut buffer = Buffer::empty(area);
        let before = buffer.clone();
        apply_glyph_animation(&mut buffer, area, AnimationState::Static, StdDuration::ZERO);
        assert_eq!(buffer, before);
    }

    #[test]
    fn applying_a_pulse_to_a_zero_size_area_does_not_panic() {
        let area = Rect::new(0, 0, 0, 0);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        apply_glyph_animation(&mut buffer, area, AnimationState::HiringPulse, StdDuration::ZERO);
        apply_glyph_animation(&mut buffer, area, AnimationState::RunningPulse, StdDuration::from_secs(3));
    }
}
