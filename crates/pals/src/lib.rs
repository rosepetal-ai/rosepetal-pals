//! PALS daemon library.
//!
//! Every daemon is a complete A2A v1.0 server built on the official Rust SDK.
//! Between peers that standard HTTP/1.1 travels unchanged inside Iroh QUIC
//! streams ([`net`]); [`a2a`] holds the server's own parts, such as the Agent
//! Card, grant authentication, task ownership, message idempotency and the
//! durable task store, [`worker`] the executors that do the work, [`blobs`]
//! the files that travel outside the control channel, [`config`] the
//! configuration file and the single-instance lock, [`gateway`] the loopback
//! entry through which standard A2A clients reach any peer, and [`node`] puts
//! them together into a running agent.

pub mod a2a;
pub mod blobs;
pub mod config;
pub mod gateway;
pub mod net;
pub mod node;
pub mod worker;
