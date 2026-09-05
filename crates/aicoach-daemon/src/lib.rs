#![allow(
    clippy::cast_possible_truncation,
    clippy::if_not_else,
    clippy::items_after_statements,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::unused_async
)]

//! Long-running AI Terminal Coach service.
//!
//! The daemon keeps terminal sessions isolated, dispatches only locally
//! selected work to an AI provider, and exposes JSON NDJSON plus the persistent
//! Zsh tab protocol over a private Unix domain socket.

pub mod capture;
pub mod runtime;
pub mod server;
pub mod state;

pub use runtime::{RuntimeFileError, RuntimeFiles, write_active_session};
pub use server::{Daemon, DaemonError, DaemonOptions};
pub use state::{
    ActiveRequestKind, AnalysisJob, CheckpointError, ConnectionId, SessionLimits, SessionManager,
    SuccessfulCommandBaseline,
};
