mod app;
mod client;
mod config;
mod effects;
mod setup;
mod ui;

use app::{App, InputMode};
use client::RpcClient;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use proto::{KeyInput, Request, Response, ResponseBody};
use reins_core::HarnessProfile;
use std::io::stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Harness profile TOML files, embedded into the binary at build time so the wizard's
/// detection pass sees the same profiles regardless of deployment layout — mirrors
/// `apps/daemon/src/main.rs`'s `load_profiles`, which the daemon needs for the same
/// reason once it's actually running.
const CLAUDE_CODE_PROFILE_TOML: &str =
    include_str!("../../../packages/adapters/profiles/claude-code.toml");
const CODEX_PROFILE_TOML: &str = include_str!("../../../packages/adapters/profiles/codex.toml");
const GEMINI_CLI_PROFILE_TOML: &str =
    include_str!("../../../packages/adapters/profiles/gemini-cli.toml");

/// Builds the adapter registry used by the first-run setup wizard's detection pass.
fn registry() -> adapters::AdapterRegistry {
    let mut registry = adapters::AdapterRegistry::new();
    registry.register(Box::new(adapters::ClaudeCodeAdapterFactory));
    registry.register(Box::new(adapters::CodexAdapterFactory));
    registry.register(Box::new(adapters::GeminiCliAdapterFactory));
    registry
}

/// Parses the embedded harness profile TOML files for the wizard's detection pass.
fn profiles() -> anyhow::Result<Vec<HarnessProfile>> {
    [
        ("claude-code.toml", CLAUDE_CODE_PROFILE_TOML),
        ("codex.toml", CODEX_PROFILE_TOML),
        ("gemini-cli.toml", GEMINI_CLI_PROFILE_TOML),
    ]
    .iter()
    .map(|(name, raw)| {
        toml::from_str::<HarnessProfile>(raw)
            .map_err(|e| anyhow::anyhow!("parsing embedded harness profile '{name}': {e}"))
    })
    .collect()
}

/// Full startup preamble, run before any terminal init so wizard/daemon-start output
/// prints cleanly to a normal (non-raw-mode) terminal: runs the first-run setup wizard
/// if the setup-complete marker is absent, then makes sure the daemon is reachable —
/// trying the installed service first, falling back to spawning `reinsd` directly.
async fn ensure_ready() -> anyhow::Result<()> {
    let marker = proto::setup_marker_path()?;
    if !marker.exists() {
        setup::run_wizard(&registry(), &profiles()?)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    let socket = proto::control_socket_path()?;
    if socket_is_alive(&socket).await {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    let started = daemon::lifecycle::systemd::start_if_installed()?;
    #[cfg(target_os = "macos")]
    let started = daemon::lifecycle::launchd::start_if_installed()?;

    if started {
        wait_for_socket(&socket).await?;
        return Ok(());
    }

    // Fallback: no service installed (or it wasn't started elsewhere) — spawn `reinsd`
    // directly, detached, so the TUI still has a daemon to talk to.
    spawn_detached_reinsd()?;
    wait_for_socket(&socket).await?;
    Ok(())
}

/// Reuses the same liveness signal the MVP's `refresh_sessions` already relies on to
/// detect "daemon unreachable" (see its `Err(err) => ... "could not reach reinsd"` arm
/// below): a failed `UnixStream::connect` means nothing is listening on the socket.
/// This checks only the connect step, not a full request/response round trip, since a
/// liveness probe doesn't need to exercise the RPC protocol.
async fn socket_is_alive(socket: &std::path::Path) -> bool {
    tokio::net::UnixStream::connect(socket).await.is_ok()
}

/// Polls the socket for liveness with a bounded number of retries (3s total) rather
/// than waiting forever, so a daemon that fails to start doesn't hang the TUI launch.
async fn wait_for_socket(socket: &std::path::Path) -> anyhow::Result<()> {
    for _ in 0..20 {
        if socket_is_alive(socket).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    anyhow::bail!("daemon did not become ready in time")
}

/// Spawns `reinsd` directly, detached from the TUI's stdio, as a last-resort fallback
/// when no installed service could be started (e.g. no systemd/launchd on this system).
fn spawn_detached_reinsd() -> anyhow::Result<()> {
    let reinsd_path = setup::resolve_reinsd_path().map_err(|e| anyhow::anyhow!("{e}"))?;
    std::process::Command::new(reinsd_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

/// Spawns background tasks that catch real OS-level SIGINT/SIGTERM (e.g. `kill`, a
/// closing terminal emulator sending SIGHUP-then-SIGTERM, `systemctl stop` if `reins`
/// were ever run as a unit) — as opposed to a Ctrl+C *keypress*, which arrives as an
/// ordinary [`Event::Key`] once raw mode is active (raw mode disables the terminal's
/// own ISIG handling, so Ctrl+C never generates a real SIGINT for us to catch).
///
/// The returned flag is set (never cleared here) whenever either signal arrives;
/// [`event_loop`] polls and clears it once per tick and feeds it into the same
/// [`App::request_quit`] confirmation flow a keyboard quit uses, rather than exiting
/// immediately on the very first signal.
#[cfg(unix)]
fn spawn_quit_signal_watcher() -> Arc<AtomicBool> {
    use tokio::signal::unix::{signal, SignalKind};

    let flag = Arc::new(AtomicBool::new(false));

    for kind in [SignalKind::interrupt(), SignalKind::terminate()] {
        let flag = flag.clone();
        // A signal stream that fails to install (e.g. this exact kind already taken
        // by something else in-process) is not fatal — the keyboard path still works.
        if let Ok(mut stream) = signal(kind) {
            tokio::spawn(async move {
                loop {
                    stream.recv().await;
                    flag.store(true, Ordering::SeqCst);
                }
            });
        }
    }

    flag
}

#[tokio::main]
async fn main() {
    // Handled before anything else — including before entering the async runtime's
    // ordinary error path — since this is a standalone elevated re-invocation, not
    // part of the normal startup flow, and must never fall through to the TUI. The
    // wizard's primary path now prompts for sudo inline (see `install_daemon` in
    // `setup/mod.rs`); this flag remains as a manual fallback for retrying just the
    // linger step standalone if that inline prompt didn't work (e.g. cancelled, or
    // the wizard ran somewhere without a real interactive terminal for `sudo`).
    if std::env::args().nth(1).as_deref() == Some("--setup-linger") {
        handle_setup_linger();
        return;
    }

    if let Err(err) = run().await {
        eprintln!("reins: {err:#}");
        std::process::exit(1);
    }
}

/// Runs `--setup-linger`: enables systemd user-linger for the current user so `reinsd`
/// (started via `systemctl --user`) keeps running across logout. The wizard normally
/// handles this itself via an inline `sudo` prompt (see `install_daemon` in
/// `setup/mod.rs`); this standalone re-invocation is a fallback for retrying just this
/// step under `sudo reins --setup-linger` if that inline prompt didn't succeed.
#[cfg(target_os = "linux")]
fn handle_setup_linger() {
    let username = match std::process::Command::new("id").arg("-un").output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(err) => {
            eprintln!("reins: could not determine current user: {err:#}");
            std::process::exit(1);
        }
    };
    match daemon::lifecycle::systemd::enable_linger(&username) {
        Ok(()) => {
            println!("linger enabled for {username}");
        }
        Err(err) => {
            eprintln!("reins: failed to enable linger: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn handle_setup_linger() {
    eprintln!("reins: --setup-linger is only needed on Linux (systemd)");
    std::process::exit(1);
}

async fn run() -> anyhow::Result<()> {
    // Handle subcommands before launching the TUI
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "config" => {
                return handle_config_subcommand(&args[2..]);
            }
            "setup" => {
                setup::run_setup(&registry(), &profiles()?);
                return Ok(());
            }
            "update" => {
                return handle_update_subcommand().await;
            }
            _ => {}
        }
    }

    // Runs before any terminal init so a wizard prompt or daemon-start message prints
    // cleanly to a normal (non-raw-mode) terminal.
    //
    // Deviation from spec/plan: the design spec (§5) and the original plan (Task 11,
    // step 4) both specify the brand splash as step 1 of the first-run experience,
    // played before the tmux/harness checks. In this assembled flow it actually runs
    // after `ensure_ready()` (which includes the first-run wizard), immediately before
    // `event_loop` below. This is because the wizard's checklist is printed with plain
    // `println!`/`eprintln!` to a normal, non-raw-mode terminal — interleaving it with
    // a ratatui-rendered splash would require either the splash to run outside raw
    // mode/the alternate screen (defeating the point of a full-screen animated splash)
    // or the wizard's plain-text output to be redrawn inside ratatui (a much larger
    // change this late in the plan). So the two effects run in the order the terminal
    // mode actually allows: plain-text wizard first, then raw-mode splash right before
    // the TUI takes over. On subsequent (non-first-run) launches this is moot, since
    // `ensure_ready()` skips the wizard entirely once the setup-complete marker exists.
    ensure_ready().await?;

    // Same resolution rules as the daemon (see proto::paths) so both ends agree
    // on a private, non-world-writable socket location.
    let socket_path = proto::control_socket_path()?;
    let rpc = RpcClient::new(socket_path);

    let mut app = App::new();
    app.animations_enabled = config::load().animations;

    if let Some(version) = daemon::updater::background_check(env!("CARGO_PKG_VERSION")).await {
        app.update_available = Some(version);
    }

    refresh_sessions(&rpc, &mut app).await;
    // The initial refresh happens before the terminal is put into raw mode, so a
    // failure here can still be reported the ordinary way on stderr. Every later
    // refresh leaves the message in `app.status_message` for the status line instead.
    if let Some(message) = app.status_message.take() {
        eprintln!("reins: {message}");
    }

    let mut terminal = init_terminal()?;
    // Splash plays here, after the wizard rather than before it — see the ordering
    // note on the `ensure_ready()` call above for why.
    effects::play_splash(&mut terminal)?;
    // Reins targets Linux and macOS only (both Unix; see the packaging spec's platform
    // scope), so this is unconditional rather than `#[cfg(unix)]`-gated at the call site.
    let quit_signal = spawn_quit_signal_watcher();

    let result = event_loop(&mut terminal, &mut app, &rpc, &quit_signal).await;
    restore_terminal(&mut terminal)?;
    result
}

/// Decision made by [`parse_config_args`]: either print the current setting, or write
/// a new `animations` value. Kept separate from `handle_config_subcommand` so the pure
/// parsing/decision logic can be unit tested without touching real config I/O or
/// stdout — mirrors the `resolve_reinsd_path`/`resolve_reinsd_path_impl` split in
/// `setup/mod.rs`.
#[derive(Debug, PartialEq, Eq)]
enum ConfigAction {
    Print,
    SetAnimations(bool),
}

/// Pure logic behind [`handle_config_subcommand`]: decides what `reins config ...`
/// should do, without performing any I/O. Takes the args following `config` on the
/// command line (i.e. `args[2..]` from `run()`).
fn parse_config_args(args: &[String]) -> anyhow::Result<ConfigAction> {
    if args.is_empty() {
        // `reins config` — print current config
        Ok(ConfigAction::Print)
    } else if args.len() >= 3 && args[0] == "set" && args[1] == "animations" {
        // `reins config set animations on|off`
        match args[2].as_str() {
            "on" => Ok(ConfigAction::SetAnimations(true)),
            "off" => Ok(ConfigAction::SetAnimations(false)),
            other => Err(anyhow::anyhow!(
                "invalid animations value: '{}'; must be 'on' or 'off'",
                other
            )),
        }
    } else {
        Err(anyhow::anyhow!(
            "unknown config subcommand; usage: reins config [set animations on|off]"
        ))
    }
}

fn handle_config_subcommand(args: &[String]) -> anyhow::Result<()> {
    match parse_config_args(args)? {
        ConfigAction::Print => {
            let cfg = config::load();
            println!("animations = {}", cfg.animations);
            Ok(())
        }
        ConfigAction::SetAnimations(value) => {
            let mut cfg = config::load();
            cfg.animations = value;
            config::save(&cfg)?;
            Ok(())
        }
    }
}

/// Runs `reins update`: checks GitHub Releases and, if a newer version exists,
/// downloads, verifies, and installs it, restarting `reinsd` in the process. Prints
/// progress to stdout as it goes — this is a synchronous CLI flow, not something the
/// TUI ever triggers on its own (see the plan's "manual install" trigger model).
async fn handle_update_subcommand() -> anyhow::Result<()> {
    let current_exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not resolve the running `reins` binary's path: {e}"))?;
    let reinsd_path = current_exe
        .parent()
        .map(|dir| dir.join("reinsd"))
        .ok_or_else(|| anyhow::anyhow!("could not resolve `reinsd`'s expected path next to `reins`"))?;

    let result = daemon::updater::run_update(
        env!("CARGO_PKG_VERSION"),
        &current_exe,
        &reinsd_path,
        |line| println!("{line}"),
    )
    .await;

    match result {
        Ok(version) if version == env!("CARGO_PKG_VERSION") => {
            println!("Already on the latest version ({version}).");
            Ok(())
        }
        Ok(version) => {
            println!("Update complete — now on {version}.");
            Ok(())
        }
        Err(err) => Err(anyhow::anyhow!("update failed: {err}")),
    }
}

fn init_terminal() -> anyhow::Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    if let Err(err) = execute!(out, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(err.into());
    }
    let backend = CrosstermBackend::new(out);
    match Terminal::new(backend) {
        Ok(terminal) => Ok(terminal),
        Err(err) => {
            let _ = execute!(stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            Err(err.into())
        }
    }
}

fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Re-fetches the session roster from the daemon and clamps the selection into range.
/// Called after every hire/release/interrupt action so the UI never shows stale state.
async fn refresh_sessions(rpc: &RpcClient, app: &mut App) {
    match rpc.send(Request::ListSessions { project_path: None }).await {
        Ok(Response::Ok { result: ResponseBody::Sessions(sessions) }) => {
            app.sessions = sessions;
            if app.selected >= app.sessions.len() {
                app.selected = app.sessions.len().saturating_sub(1);
            }
            app.sync_hire_tracking();
        }
        Ok(Response::Ok { .. }) => {
            // Unexpected but non-fatal: leave the roster as-is.
        }
        Ok(Response::Err { message }) => {
            app.set_status_message(format!("daemon returned an error: {message}"));
        }
        Err(err) => {
            app.set_status_message(format!("could not reach reinsd (is it running?): {err:#}"));
        }
    }
}

/// Polls the daemon for the selected session's latest pane content (color-coded text
/// plus cursor position — see `ResponseBody::PaneSnapshot`). Best-effort: errors are
/// silently ignored so a transient RPC hiccup doesn't interrupt the UI.
async fn refresh_pane(rpc: &RpcClient, app: &mut App) {
    let Some(session) = app.selected_session() else {
        app.pane_content.clear();
        return;
    };
    let session_id = session.id.clone();
    if let Ok(Response::Ok { result: ResponseBody::PaneSnapshot { text, cursor } }) =
        rpc.send(Request::GetPaneSnapshot { session_id }).await
    {
        app.pane_content = text;
        app.pane_cursor = cursor;
    }
}

/// Pane poll interval outside focus mode — comfortably inside the 200-500ms target
/// from the brief.
const PANE_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Pane poll interval while focused on a session's pane: quick enough that typed
/// input and the harness's response both feel live, not a periodic refresh.
const FOCUSED_PANE_POLL_INTERVAL: Duration = Duration::from_millis(80);

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    rpc: &RpcClient,
    quit_signal: &AtomicBool,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // An external SIGINT/SIGTERM (see `spawn_quit_signal_watcher`) goes through
        // the exact same confirm-to-quit flow a keyboard quit does, rather than
        // exiting on the very first signal. Suppressed while focused: a signal arriving
        // mid-focus shouldn't fight with keystrokes meant for the harness — the prefix
        // chord (Ctrl-B d) is the way out of focus mode, matching how 'q'/Ctrl+C are
        // also forwarded to the pane instead of quitting reins while focused.
        if !app.is_focused() && quit_signal.swap(false, Ordering::SeqCst) && app.request_quit() {
            break;
        }

        // event::poll blocks (synchronously) for up to this long, which doubles as our
        // pane-refresh tick. Shorter while focused so typing feels live.
        let poll_interval =
            if app.is_focused() { FOCUSED_PANE_POLL_INTERVAL } else { PANE_POLL_INTERVAL };
        if event::poll(poll_interval)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if app.is_focused() {
                    handle_focused_mode_key(key, app, rpc).await;
                } else {
                    // Any keypress dismisses a stale status message; the handlers
                    // below then set a fresh one if this action fails too. The quit
                    // warning itself is separate state (`App::quit_warning_active`),
                    // not routed through `status_message`, so clearing this first
                    // doesn't clobber it. Skipped in focus mode above: there, 'q' and
                    // Ctrl+C are keystrokes for the harness, not a reins command.
                    app.clear_status_message();

                    let is_quit_key = app.input_mode.is_none()
                        && (key.code == KeyCode::Char('q')
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL)));

                    if is_quit_key {
                        if app.request_quit() {
                            break;
                        }
                    } else if app.input_mode.is_some() {
                        handle_input_mode_key(key.code, app, rpc).await;
                    } else {
                        handle_normal_mode_key(key.code, app, rpc).await;
                    }
                }
            }
        }

        refresh_pane(rpc, app).await;
    }
    Ok(())
}

/// Handles a keypress while focused on a session's pane: the focus-mode prefix chord
/// (`Ctrl-B`) claims the *next* keystroke as a reins command (currently only `d`,
/// defocus — mirroring tmux's own detach chord, since anyone using reins already has
/// tmux sessions underneath it) rather than forwarding it; every other keystroke,
/// `q`/Ctrl+C/arrows included, is translated and sent straight into the pane.
async fn handle_focused_mode_key(key: crossterm::event::KeyEvent, app: &mut App, rpc: &RpcClient) {
    if !app.take_prefix_pending() {
        if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
            app.arm_prefix();
            return;
        }
    } else {
        // This keystroke follows a prefix chord: it's a reins command, or (matching
        // tmux's own behavior for an unbound prefix key) silently dropped rather than
        // forwarded — a partial prefix chord should never leak into the pane as a
        // stray 'd' or whatever else was pressed.
        if key.code == KeyCode::Char('d') {
            app.exit_focus();
        }
        return;
    }

    let Some(session_id) = app.selected_session().map(|s| s.id.clone()) else {
        return;
    };
    let Some(input) = key_event_to_input(key) else {
        return;
    };
    if let Err(err) = rpc.send(Request::SendKeys { session_id, input }).await {
        app.set_status_message(format!("could not send input: {err:#}"));
    }
}

/// Translates one crossterm key event into the wire representation
/// `Request::SendKeys` expects — printable characters become literal text
/// (`send-keys -l`), everything else maps onto tmux's own named-key vocabulary
/// (`"Enter"`, `"Left"`, `"C-c"`, ...) rather than reins hand-rolling ANSI escape
/// sequences itself. Returns `None` for key codes with no tmux equivalent (e.g. media
/// keys) — dropped rather than guessed at.
fn key_event_to_input(key: crossterm::event::KeyEvent) -> Option<KeyInput> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char(c) if ctrl => {
            Some(KeyInput::Named { token: format!("C-{}", c.to_ascii_lowercase()) })
        }
        KeyCode::Char(c) if alt => {
            Some(KeyInput::Named { token: format!("M-{}", c.to_ascii_lowercase()) })
        }
        KeyCode::Char(c) => Some(KeyInput::Literal { text: c.to_string() }),
        KeyCode::Enter => Some(KeyInput::Named { token: "Enter".into() }),
        KeyCode::Backspace => Some(KeyInput::Named { token: "BSpace".into() }),
        KeyCode::Tab => Some(KeyInput::Named { token: "Tab".into() }),
        KeyCode::BackTab => Some(KeyInput::Named { token: "BTab".into() }),
        KeyCode::Esc => Some(KeyInput::Named { token: "Escape".into() }),
        KeyCode::Left => Some(KeyInput::Named { token: "Left".into() }),
        KeyCode::Right => Some(KeyInput::Named { token: "Right".into() }),
        KeyCode::Up => Some(KeyInput::Named { token: "Up".into() }),
        KeyCode::Down => Some(KeyInput::Named { token: "Down".into() }),
        KeyCode::Home => Some(KeyInput::Named { token: "Home".into() }),
        KeyCode::End => Some(KeyInput::Named { token: "End".into() }),
        KeyCode::PageUp => Some(KeyInput::Named { token: "PageUp".into() }),
        KeyCode::PageDown => Some(KeyInput::Named { token: "PageDown".into() }),
        KeyCode::Delete => Some(KeyInput::Named { token: "Delete".into() }),
        KeyCode::F(n) => Some(KeyInput::Named { token: format!("F{n}") }),
        _ => None,
    }
}

/// Optional text fields in the hire prompt: an empty buffer means "not supplied".
fn none_if_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Handles a keypress while the inline hire prompt is active.
async fn handle_input_mode_key(code: KeyCode, app: &mut App, rpc: &RpcClient) {
    match code {
        KeyCode::Esc => app.cancel_input(),
        // Up/down move the harness picker's highlight during that step; everywhere
        // else in the prompt these keys don't do anything (only Role/Brief accept
        // free text, via push_char/backspace below).
        KeyCode::Up if app.input_mode == Some(InputMode::HarnessId) => app.picker_prev(),
        KeyCode::Down if app.input_mode == Some(InputMode::HarnessId) => app.picker_next(),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Char(c) => app.push_char(c),
        KeyCode::Enter => {
            if let Some(input) = app.advance_input() {
                // An emptied-out working-directory field (e.g. backspaced away
                // entirely) falls back to reins' own current directory, same as the
                // pre-picker default — never sends an empty project_path.
                let project_path = none_if_empty(input.working_dir).unwrap_or_else(|| {
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| ".".to_string())
                });
                let req = Request::Hire {
                    harness_id: input.harness_id,
                    project_path,
                    role: none_if_empty(input.role),
                    brief: none_if_empty(input.brief),
                };
                match rpc.send(req).await {
                    Ok(Response::Err { message }) => {
                        app.set_status_message(format!("hire failed: {message}"))
                    }
                    Err(err) => app.set_status_message(format!("hire failed: {err:#}")),
                    Ok(Response::Ok { .. }) => {}
                }
                refresh_sessions(rpc, app).await;
            }
        }
        _ => {}
    }
}

/// Handles a keypress while no inline prompt is active (`q` is handled by the caller).
async fn handle_normal_mode_key(code: KeyCode, app: &mut App, rpc: &RpcClient) {
    match code {
        KeyCode::Down => app.select_next(),
        KeyCode::Up => app.select_prev(),
        KeyCode::Enter => {
            if !app.enter_focus() {
                app.set_status_message("select a team member first (up/down)");
            }
        }
        KeyCode::Char('h') => {
            match rpc.send(Request::ListHarnesses).await {
                Ok(Response::Ok { result: ResponseBody::Harnesses(harnesses) }) => {
                    let default_working_dir = std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if !app.start_hire_input(harnesses, default_working_dir) {
                        app.set_status_message(
                            "no available harness CLIs — install one and run `reins setup`",
                        );
                    }
                }
                Ok(Response::Err { message }) => {
                    app.set_status_message(format!("could not list harnesses: {message}"));
                }
                Ok(Response::Ok { .. }) => {}
                Err(err) => {
                    app.set_status_message(format!("could not reach reinsd: {err:#}"));
                }
            }
        }
        KeyCode::Char('r') => {
            if let Some(session_id) = app.selected_session().map(|s| s.id.clone()) {
                let response = rpc.send(Request::Release { session_id }).await;
                report_action_result(app, "release", response);
                refresh_sessions(rpc, app).await;
            }
        }
        KeyCode::Char('i') => {
            if let Some(session_id) = app.selected_session().map(|s| s.id.clone()) {
                let response = rpc.send(Request::Interrupt { session_id }).await;
                report_action_result(app, "interrupt", response);
                refresh_sessions(rpc, app).await;
            }
        }
        _ => {}
    }
}

/// Surfaces a failed roster action in the status line. Nothing is printed to stderr:
/// the terminal is in raw mode on the alternate screen, so stderr writes would corrupt
/// the rendered frame.
fn report_action_result(app: &mut App, action: &str, response: anyhow::Result<Response>) {
    match response {
        Ok(Response::Err { message }) => {
            app.set_status_message(format!("{action} failed: {message}"))
        }
        Err(err) => app.set_status_message(format!("{action} failed: {err:#}")),
        Ok(Response::Ok { .. }) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, modifiers)
    }

    #[test]
    fn key_event_to_input_sends_plain_characters_as_literal_text() {
        let input = key_event_to_input(key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        assert_eq!(input, KeyInput::Literal { text: "a".into() });
    }

    #[test]
    fn key_event_to_input_maps_ctrl_chars_to_tmux_named_tokens() {
        let input = key_event_to_input(key(KeyCode::Char('c'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(input, KeyInput::Named { token: "C-c".into() });
    }

    #[test]
    fn key_event_to_input_maps_alt_chars_to_meta_tokens() {
        let input = key_event_to_input(key(KeyCode::Char('b'), KeyModifiers::ALT)).unwrap();
        assert_eq!(input, KeyInput::Named { token: "M-b".into() });
    }

    #[test]
    fn key_event_to_input_maps_special_keys_to_tmux_key_names() {
        let cases = [
            (KeyCode::Enter, "Enter"),
            (KeyCode::Backspace, "BSpace"),
            (KeyCode::Tab, "Tab"),
            (KeyCode::BackTab, "BTab"),
            (KeyCode::Esc, "Escape"),
            (KeyCode::Left, "Left"),
            (KeyCode::Right, "Right"),
            (KeyCode::Up, "Up"),
            (KeyCode::Down, "Down"),
            (KeyCode::Home, "Home"),
            (KeyCode::End, "End"),
            (KeyCode::PageUp, "PageUp"),
            (KeyCode::PageDown, "PageDown"),
            (KeyCode::Delete, "Delete"),
        ];
        for (code, expected_token) in cases {
            let input = key_event_to_input(key(code, KeyModifiers::NONE))
                .unwrap_or_else(|| panic!("{code:?} should map to a token"));
            assert_eq!(input, KeyInput::Named { token: expected_token.into() }, "for {code:?}");
        }
    }

    #[test]
    fn key_event_to_input_maps_function_keys() {
        let input = key_event_to_input(key(KeyCode::F(5), KeyModifiers::NONE)).unwrap();
        assert_eq!(input, KeyInput::Named { token: "F5".into() });
    }

    #[test]
    fn key_event_to_input_drops_keys_with_no_tmux_equivalent() {
        assert_eq!(key_event_to_input(key(KeyCode::Menu, KeyModifiers::NONE)), None);
    }

    #[test]
    fn config_no_args_prints_current_setting() {
        let result = parse_config_args(&args(&[])).expect("no-args should be valid");
        assert_eq!(result, ConfigAction::Print);
    }

    #[test]
    fn config_set_animations_on() {
        let result =
            parse_config_args(&args(&["set", "animations", "on"])).expect("'on' should be valid");
        assert_eq!(result, ConfigAction::SetAnimations(true));
    }

    #[test]
    fn config_set_animations_off() {
        let result = parse_config_args(&args(&["set", "animations", "off"]))
            .expect("'off' should be valid");
        assert_eq!(result, ConfigAction::SetAnimations(false));
    }

    #[test]
    fn config_set_animations_invalid_value_errors_clearly() {
        let err = parse_config_args(&args(&["set", "animations", "maybe"]))
            .expect_err("invalid value should be rejected");
        let message = err.to_string();
        assert!(
            message.contains("invalid animations value"),
            "error message should explain the problem, got: {message}"
        );
        assert!(
            message.contains("maybe"),
            "error message should include the offending value, got: {message}"
        );
    }

    #[test]
    fn config_unknown_subcommand_errors_clearly() {
        let err = parse_config_args(&args(&["bogus"]))
            .expect_err("unknown subcommand should be rejected");
        let message = err.to_string();
        assert!(
            message.contains("unknown config subcommand"),
            "error message should explain the problem, got: {message}"
        );
    }

    #[test]
    fn config_set_missing_value_errors() {
        let err = parse_config_args(&args(&["set", "animations"]))
            .expect_err("missing value should be rejected");
        let message = err.to_string();
        assert!(
            message.contains("unknown config subcommand"),
            "error message should explain the problem, got: {message}"
        );
    }
}
