//! Stable IPC types and transports shared by the daemon, CLI, TUI and Zsh.
//!
//! Rust clients use one JSON object per line over a Unix domain socket. The
//! persistent Zsh integration uses the smaller tab protocol documented in
//! [`zsh`]. Both protocols carry explicit request and session identifiers.

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::struct_excessive_bools,
    clippy::too_many_lines
)]

pub mod client;
pub mod protocol;
pub mod transport;
pub mod wire;
pub mod zsh;

pub use client::{ClientError, IpcClient};
pub use protocol::*;
pub use transport::{NdjsonReader, NdjsonWriter, TransportError};
pub use wire::{WireError, WireProtocol, decode_incoming, encode_outgoing};
