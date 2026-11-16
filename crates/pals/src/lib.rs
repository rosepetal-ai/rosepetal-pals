//! PALS daemon library.
//!
//! Every daemon is a complete A2A v1.0 server built on the official Rust SDK.
//! Between peers that standard HTTP/1.1 travels unchanged inside Iroh QUIC
//! streams ([`net`]); [`a2a`] holds the server's own parts, such as the Agent
//! Card, grant authentication, task ownership, message idempotency and the
//! durable task store, [`worker`] the executors that do the work, [`blobs`]
//! the files that travel outside the control channel, [`config`] the
//! configuration file and the single-instance lock, [`inventory`] what the
//! agent offers and with what, as it serves it, [`discovery`] peers'
//! verified cards and the agent's own signed records, [`gateway`] the loopback
//! entry through which standard A2A clients reach any peer, [`outbox`] the
//! questions this agent asks them, [`mcp`] the tools an AI application on
//! this machine uses them with, and [`node`] puts them together into a
//! running agent.

pub mod a2a;
pub mod blobs;
pub mod config;
pub mod discovery;
pub mod gateway;
pub mod inventory;
pub mod mcp;
pub mod net;
pub mod node;
pub mod outbox;
mod path_policy;
#[cfg(unix)]
mod process_group;
pub mod worker;

/// `text`, which may come from a peer, as printable ASCII, spaces kept and
/// everything else dropped, cut to its first `max_chars`: the one way a
/// peer's words are shown, in an error, a log or a command's output, so
/// that they never carry a control sequence or a line of their own.
pub fn printable_ascii(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(max_chars)
        .collect()
}

/// Installs `ring` as the process's rustls crypto provider, unless one is
/// installed already.
///
/// iroh builds reqwest with rustls and no provider, so every HTTP client
/// built on it, the A2A client's included, needs one installed first: call
/// this before building any.
pub fn install_crypto_provider() {
    // Another caller may have installed it already.
    let _ = rustls::crypto::ring::default_provider().install_default();
}
