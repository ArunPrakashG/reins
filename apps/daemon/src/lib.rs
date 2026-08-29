//! Library surface for `daemon`, split out from the `reinsd` binary so that
//! external integration tests (in `tests/`) — and any future embedder — can drive the
//! control server through its real public API instead of duplicating internals.
//!
//! `main.rs` is a thin wrapper around this crate: it wires up the concrete
//! `AdapterRegistry`/`TmuxController`/`SqliteStore` pieces and calls
//! [`rpc_server::run_control_server`].

pub mod lifecycle;
pub mod rpc_server;
pub mod session_manager;
pub mod tmux;

pub use session_manager::{SessionManager, SessionManagerError};
pub use tmux::{TmuxController, TmuxError};
