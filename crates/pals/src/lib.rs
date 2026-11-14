//! PALS daemon library.
//!
//! Every daemon is a complete A2A v1.0 server built on the official Rust SDK.
//! Between peers that standard HTTP/1.1 travels unchanged inside Iroh QUIC
//! streams ([`net`]); [`worker`] holds the executors that do the work, and
//! [`config`] the configuration file and the single-instance lock.

pub mod config;
pub mod net;
pub mod worker;
