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
    match rpc.send(Request::ListSessions { project_path: None }).await {
        Ok(Response::Ok { result: ResponseBody::Sessions(sessions) }) => {
            app.sessions = sessions;
        }
        Ok(Response::Ok { .. }) => {
            // Unexpected but non-fatal: leave the roster empty.
        }
        Ok(Response::Err { message }) => {
            eprintln!("reins: daemon returned an error: {message}");
        }
        Err(err) => {
            eprintln!(
                "reins: could not reach reinsd (is it running?): {err:#}"
            );
            return Ok(());
        }
    }

    let mut terminal = init_terminal()?;
    let result = event_loop(&mut terminal, &mut app);
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

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Down => app.select_next(),
                    KeyCode::Up => app.select_prev(),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
