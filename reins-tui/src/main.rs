mod app;
mod client;
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
use reins_proto::{Request, Response, ResponseBody};
use std::io::stdout;
use std::time::Duration;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("reins: {err:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let socket_path = std::env::temp_dir().join("reinsd.sock");
    let rpc = RpcClient::new(socket_path);

    let mut app = App::new();
    refresh_sessions(&rpc, &mut app).await;

    let mut terminal = init_terminal()?;
    let result = event_loop(&mut terminal, &mut app, &rpc).await;
    restore_terminal(&mut terminal)?;
    result
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
            eprintln!("reins: daemon returned an error: {message}");
        }
        Err(err) => {
            eprintln!("reins: could not reach reinsd (is it running?): {err:#}");
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

/// Handles a keypress while the two-step inline hire prompt is active.
async fn handle_input_mode_key(code: KeyCode, app: &mut App, rpc: &RpcClient) {
    match code {
        KeyCode::Esc => app.cancel_input(),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Char(c) => app.push_char(c),
        KeyCode::Enter => {
            if let Some((harness_id, role)) = app.advance_input() {
                let project_path = std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string());
                let req = Request::Hire {
                    harness_id,
                    project_path,
                    role: if role.is_empty() { None } else { Some(role) },
                    brief: None,
                };
                if let Ok(Response::Err { message }) = rpc.send(req).await {
                    eprintln!("reins: hire failed: {message}");
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
                let _ = rpc.send(Request::Release { session_id }).await;
                refresh_sessions(rpc, app).await;
            }
        }
        KeyCode::Char('i') => {
            if let Some(session_id) = app.selected_session().map(|s| s.id.clone()) {
                let _ = rpc.send(Request::Interrupt { session_id }).await;
                refresh_sessions(rpc, app).await;
            }
        }
        _ => {}
    }
}
