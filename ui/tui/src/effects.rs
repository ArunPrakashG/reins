//! Branded ASCII-wordmark splash, played once on every `reins` launch (Task 11).
//!
//! Uses `tachyonfx` 0.7 (pinned to `ratatui = "0.28.1"`, matching this workspace's own
//! `ratatui = "0.28"` pin exactly — every later tachyonfx release requires `ratatui`
//! 0.29+ and the post-split `ratatui-core` crate, which is incompatible with this
//! workspace) to animate the wordmark in with a `coalesce` entrance effect.

use crossterm::event::{self, Event};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Color;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io::Stdout;
use std::time::{Duration as StdDuration, Instant};
use tachyonfx::{fx, Effect, EffectRenderer, EffectTimer, Interpolatable, Interpolation, Shader};

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

/// Full period (fade up *and* back down) of the hiring pulse.
const HIRING_PULSE_PERIOD_MS: u64 = 1200;

/// Full period of the running pulse — slower and subtler than the hiring pulse so the
/// two read as visually distinct at a glance.
const RUNNING_PULSE_PERIOD_MS: u64 = 2400;

/// Peak color the hiring pulse breathes towards.
const HIRING_PULSE_PEAK: Color = Color::Yellow;

/// Peak color the running pulse breathes towards.
const RUNNING_PULSE_PEAK: Color = Color::Green;

/// Base (trough) color both pulses breathe from/to — the terminal's default foreground,
/// matching the MVP's plain, uncolored glyph text.
const PULSE_BASE: Color = Color::Reset;

/// Easing used for both legs of the triangle wave.
const PULSE_INTERPOLATION: Interpolation = Interpolation::SineInOut;

/// Computes a symmetric "breathing" alpha (0.0 at the trough, 1.0 at the peak, 0.0 again
/// a full period later) for a point `elapsed` into a looping pulse of the given `period`,
/// eased by `interpolation`. Pure function of `elapsed`/`period` — no cumulative state,
/// so it's trivially safe to call every frame with a growing `elapsed` and get a stable,
/// correctly-looping result.
///
/// This deliberately does **not** use `tachyonfx`'s own `fx::ping_pong`/`fx::fade_to_fg`
/// composition for the breathing motion. Two independent bugs in that combination were
/// found in review:
/// 1. `fx::ping_pong`'s `process()` treats its `duration` argument as a delta to
///    subtract from remaining time, and explicitly discards any overflow when it
///    reverses direction (see `tachyonfx`'s own `ping_pong.rs`: `// consumes any
///    overflow when reversing, to reset the area`) — so driving it with one large
///    "absolute phase" `process()` call (Task 12's original approach) fades in
///    correctly, then freezes solid at peak color for the entire second leg.
/// 2. Less obviously: even switching to small, real per-frame deltas on a persistent
///    `Effect` (the fix that was tried next) does **not** actually fix it, because
///    `fx::fade_to_fg`'s `execute()` re-reads the buffer cell's *current* (already
///    mutated) color as its interpolation source on every call, rather than the color
///    it started from. By the time the reverse leg begins, the cell is already at (or
///    extremely close to) peak color, and `peak_color.lerp(peak_color, alpha)` is
///    `peak_color` for any `alpha` — so it never visibly fades back down, confirmed by
///    direct instrumentation (`fg` samples pinned at `Yellow` through the whole second
///    leg regardless of delta size).
///
/// Computing the eased triangle wave by hand and writing the resulting color directly
/// sidesteps both bugs: there's no mutable `Effect` state to get out of sync with the
/// buffer, and the "source" color for the lerp is always the fixed `PULSE_BASE`, never
/// whatever the buffer happened to hold last frame.
fn pulse_alpha(elapsed: StdDuration, period_ms: u64, interpolation: Interpolation) -> f32 {
    let period_ms = period_ms.max(1);
    let half_ms = (period_ms / 2).max(1);
    let phase_ms = (elapsed.as_millis() as u64) % period_ms;
    // Triangle wave: rises 0.0->1.0 over the first half, falls 1.0->0.0 over the second.
    let t = if phase_ms <= half_ms {
        phase_ms as f32 / half_ms as f32
    } else {
        (period_ms - phase_ms) as f32 / half_ms as f32
    };
    interpolation.alpha(t.clamp(0.0, 1.0))
}

/// Applies the glyph animation for one roster row directly to `buffer` at `area` (which
/// should cover just the status-glyph cell, not the whole row). `elapsed` is wall-clock
/// time since the row's animation "began" — hire time for
/// [`AnimationState::HiringPulse`], app start for [`AnimationState::RunningPulse`]. See
/// [`pulse_alpha`]'s doc comment for why this computes the color directly instead of
/// driving a `tachyonfx` `Effect`.
pub fn apply_glyph_animation(
    buffer: &mut ratatui::buffer::Buffer,
    area: Rect,
    state: AnimationState,
    elapsed: StdDuration,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (peak, period_ms) = match state {
        AnimationState::Static => return,
        AnimationState::HiringPulse => (HIRING_PULSE_PEAK, HIRING_PULSE_PERIOD_MS),
        AnimationState::RunningPulse => (RUNNING_PULSE_PEAK, RUNNING_PULSE_PERIOD_MS),
    };
    let alpha = pulse_alpha(elapsed, period_ms, PULSE_INTERPOLATION);
    let color = PULSE_BASE.lerp(&peak, alpha);
    if let Some(cell) = buffer.cell_mut((area.x, area.y)) {
        cell.set_fg(color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

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
    fn pulse_alpha_rises_then_falls_across_a_full_cycle() {
        // Regression coverage for the bug the task reviewer caught: composing
        // `fx::ping_pong` with `fx::fade_to_fg` looked fine for the first half of a
        // cycle but froze at peak color for the second half — confirmed by direct
        // instrumentation, and true regardless of whether it was driven by one large
        // "absolute phase" call or by small real per-frame deltas (see `pulse_alpha`'s
        // doc comment for the full root-cause explanation of both failure modes). This
        // hand-computed triangle wave sidesteps both bugs entirely, and this test
        // verifies it actually rises AND falls across one full period.
        let period_ms = 1200u64;
        let samples: Vec<f32> = (0..=period_ms)
            .step_by(100)
            .map(|ms| pulse_alpha(StdDuration::from_millis(ms), period_ms, Interpolation::Linear))
            .collect();

        // Starts at the trough...
        assert!(samples[0] < 0.05, "expected ~0.0 at phase 0, got {}", samples[0]);
        // ...rises to the peak at the midpoint...
        let mid_index = samples.len() / 2;
        assert!(samples[mid_index] > 0.95, "expected ~1.0 at the midpoint, got {}", samples[mid_index]);
        // ...and falls back to the trough by the end of the cycle.
        let last = *samples.last().unwrap();
        assert!(last < 0.05, "expected ~0.0 back at a full period, got {last}");

        // The whole second half must be strictly decreasing, not frozen at the peak —
        // this is exactly the property the buggy tachyonfx composition violated.
        let second_half = &samples[mid_index..];
        for pair in second_half.windows(2) {
            assert!(
                pair[1] <= pair[0] + f32::EPSILON,
                "expected the second half of the cycle to keep falling, got {second_half:?}"
            );
        }
        let second_half_distinct: std::collections::HashSet<_> =
            second_half.iter().map(|a| (a * 1000.0) as i32).collect();
        assert!(
            second_half_distinct.len() > 1,
            "expected the second leg to keep changing instead of freezing: {second_half:?}"
        );
    }

    #[test]
    fn pulse_alpha_loops_correctly_past_a_full_period() {
        // A second full cycle should look exactly like the first — this is what makes
        // it safe to call every frame with an ever-growing `elapsed` for as long as a
        // session stays `Starting`/`Running`, without needing to reset any state.
        let period_ms = 1200u64;
        let a1 = pulse_alpha(StdDuration::from_millis(300), period_ms, Interpolation::Linear);
        let a2 = pulse_alpha(StdDuration::from_millis(300 + period_ms * 5), period_ms, Interpolation::Linear);
        assert_eq!(a1, a2);
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
    fn applying_a_hiring_pulse_moves_the_glyph_color_through_a_full_cycle() {
        let area = Rect::new(0, 0, 1, 1);
        let mut buffer = Buffer::empty(area);

        apply_glyph_animation(&mut buffer, area, AnimationState::HiringPulse, StdDuration::ZERO);
        let trough = buffer.cell((0, 0)).unwrap().fg;

        apply_glyph_animation(
            &mut buffer,
            area,
            AnimationState::HiringPulse,
            StdDuration::from_millis(HIRING_PULSE_PERIOD_MS / 2),
        );
        let peak = buffer.cell((0, 0)).unwrap().fg;
        assert_eq!(peak, HIRING_PULSE_PEAK);
        assert_ne!(peak, trough);

        apply_glyph_animation(
            &mut buffer,
            area,
            AnimationState::HiringPulse,
            StdDuration::from_millis(HIRING_PULSE_PERIOD_MS - 1),
        );
        let near_end = buffer.cell((0, 0)).unwrap().fg;
        // Deep into the second leg, it must have moved away from peak again — this is
        // exactly the property the buggy tachyonfx composition failed to have.
        assert_ne!(near_end, peak);
    }

    #[test]
    fn applying_a_pulse_to_a_zero_size_area_does_not_panic() {
        let area = Rect::new(0, 0, 0, 0);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        apply_glyph_animation(&mut buffer, area, AnimationState::HiringPulse, StdDuration::ZERO);
        apply_glyph_animation(&mut buffer, area, AnimationState::RunningPulse, StdDuration::from_secs(3));
    }
}
