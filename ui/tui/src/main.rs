mod app;
mod client;
mod config;
mod setup;
mod ui;

use app::App;
use client::RpcClient;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use proto::{Request, Response, ResponseBody};
use reins_core::HarnessProfile;
use std::io::stdout;
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

#[tokio::main]
async fn main() {
    // Handled before anything else — including before entering the async runtime's
    // ordinary error path — since this is a standalone elevated re-invocation
    // (`sudo reins --setup-linger`) run after the wizard printed the instruction, not
    // part of the normal startup flow, and must never fall through to the TUI.
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
/// (started via `systemctl --user`) keeps running across logout. This needs elevated
/// privileges the wizard itself doesn't have, so it's a separate, explicit re-invocation
/// (`sudo reins --setup-linger`) rather than re-running the whole wizard under sudo.
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
            _ => {}
        }
    }

    // Runs before any terminal init so a wizard prompt or daemon-start message prints
    // cleanly to a normal (non-raw-mode) terminal.
    ensure_ready().await?;

    // Same resolution rules as the daemon (see proto::paths) so both ends agree
    // on a private, non-world-writable socket location.
    let socket_path = proto::control_socket_path()?;
    let rpc = RpcClient::new(socket_path);

    let mut app = App::new();
    refresh_sessions(&rpc, &mut app).await;
    // The initial refresh happens before the terminal is put into raw mode, so a
    // failure here can still be reported the ordinary way on stderr. Every later
    // refresh leaves the message in `app.status_message` for the status line instead.
    if let Some(message) = app.status_message.take() {
        eprintln!("reins: {message}");
    }

    let mut terminal = init_terminal()?;
    let result = event_loop(&mut terminal, &mut app, &rpc).await;
    restore_terminal(&mut terminal)?;
    result
}

fn handle_config_subcommand(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        // `reins config` — print current config
        let cfg = config::load();
        println!("animations = {}", cfg.animations);
        Ok(())
    } else if args.len() >= 3 && args[0] == "set" && args[1] == "animations" {
        // `reins config set animations on|off`
        let value = match args[2].as_str() {
            "on" => true,
            "off" => false,
            other => {
                return Err(anyhow::anyhow!(
                    "invalid animations value: '{}'; must be 'on' or 'off'",
                    other
                ));
            }
        };
        let mut cfg = config::load();
        cfg.animations = value;
        config::save(&cfg)?;
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "unknown config subcommand; usage: reins config [set animations on|off]"
        ))
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

/// Polls the daemon for the selected session's latest tmux pane text. Best-effort:
/// errors are silently ignored so a transient RPC hiccup doesn't interrupt the UI.
async fn refresh_pane(rpc: &RpcClient, app: &mut App) {
    let Some(session) = app.selected_session() else {
        app.pane_content.clear();
        return;
    };
    let session_id = session.id.clone();
    if let Ok(Response::Ok { result: ResponseBody::PaneSnapshot(text) }) =
        rpc.send(Request::GetPaneSnapshot { session_id }).await
    {
        app.pane_content = text;
    }
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    rpc: &RpcClient,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // event::poll blocks (synchronously) for up to this long, which doubles as our
        // pane-refresh tick — comfortably inside the 200-500ms target from the brief.
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                // Any keypress dismisses a stale status message; the handlers below
                // then set a fresh one if this action fails too.
                app.clear_status_message();
                if app.input_mode.is_some() {
                    handle_input_mode_key(key.code, app, rpc).await;
                } else if key.code == KeyCode::Char('q') {
                    break;
                } else {
                    handle_normal_mode_key(key.code, app, rpc).await;
                }
            }
        }

        refresh_pane(rpc, app).await;
    }
    Ok(())
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
        KeyCode::Backspace => app.backspace(),
        KeyCode::Char(c) => app.push_char(c),
        KeyCode::Enter => {
            if let Some(input) = app.advance_input() {
                let project_path = std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string());
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
        KeyCode::Char('h') => app.start_hire_input(),
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
