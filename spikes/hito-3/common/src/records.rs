//! The records a node publishes and how a searcher checks what it gets.
//!
//! Every candidate carries `pals-core`'s `agentnet.contact/1` and
//! `agentnet.announce/1` records unchanged, as the JSON of their
//! [`Signed`] form. [`issue_contact`] and [`issue_announce`] make a node's
//! own, with the 24 h lifetime of `pals-core` ([`RECORD_TTL_S`]): the DHT
//! applies its own, shorter one (`--ttl-s`). What a backend's search returns
//! is untrusted: [`verify`] checks each pair and [`dedupe`] keeps one
//! candidate per agent (spec §5.3, step 4).

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pals_core::card_jws::CardHash;
use pals_core::discovery::ALPN_DISCOVERY;
use pals_core::grant::format_timestamp;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::records::{
    ANNOUNCE_SCHEMA, AnnounceRecord, CONTACT_SCHEMA, ContactRecord, IrohContact, MAX_DIRECT_ADDRS,
    RECORD_TTL_S,
};
use pals_core::signed::{Signed, check_validity};
use serde::Serialize;

/// The contact does not parse or its signature does not verify, or there is
/// none.
pub const BAD_CONTACT: &str = "bad_contact";
/// The announce does not parse or its signature does not verify, or there
/// is none.
pub const BAD_ANNOUNCE: &str = "bad_announce";
/// The contact and the announce are of different agents.
pub const AGENT_MISMATCH: &str = "agent_mismatch";
/// The announce does not list the capability searched.
pub const CAPABILITY_MISSING: &str = "capability_missing";
/// The contact or the announce is outside its validity window.
pub const EXPIRED: &str = "expired";

/// There is no Agent Card in the spikes, so the announce names none: any
/// well-formed hash will do, and every node uses this one.
const NO_CARD: &str = "blake3:0000000000000000000000000000000000000000000000000000000000000000";

/// One provider as a backend's search found it: the JSON of a
/// `Signed<ContactRecord>` and of a `Signed<AnnounceRecord>`, each as it
/// arrived, or `None` when the backend did not get it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawCandidate {
    pub contact: Option<Vec<u8>>,
    pub announce: Option<Vec<u8>>,
}

/// What a backend's search returns: every candidate it found, unverified,
/// and how many requests the search made when the backend counts them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawSearch {
    pub candidates: Vec<RawCandidate>,
    pub requests: Option<u64>,
}

/// A candidate once [`verify`] checked it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// The agent of the announce, or of the contact when the announce does
    /// not verify; `None` when neither does.
    pub agent_id: Option<AgentId>,
    pub valid: bool,
    /// Why it is not valid, one of the constants of this module; `None`
    /// when it is.
    pub reason: Option<&'static str>,
    /// The verified contact's direct addresses, empty unless `valid`: what
    /// tells a searcher that a provider moved (scenario S7).
    pub direct_addrs: Vec<String>,
}

/// Checks one candidate found for `capability` at `now_unix`: its contact
/// and announce verify with `Signed::from_json` (signature, canonical form,
/// the record's own rules), are of the same agent, the announce lists
/// `capability`, and both are inside their validity window, with
/// `pals-core`'s clock skew. The first check that fails gives the reason.
pub fn verify(capability: &str, raw: RawCandidate, now_unix: i64) -> Candidate {
    let contact = raw
        .contact
        .as_deref()
        .and_then(|bytes| Signed::<ContactRecord>::from_json(bytes).ok());
    let announce = raw
        .announce
        .as_deref()
        .and_then(|bytes| Signed::<AnnounceRecord>::from_json(bytes).ok());
    let agent_id = announce
        .as_ref()
        .map(|a| a.doc.agent_id)
        .or_else(|| contact.as_ref().map(|c| c.doc.agent_id));
    let reject = |reason| Candidate {
        agent_id,
        valid: false,
        reason: Some(reason),
        direct_addrs: Vec::new(),
    };

    let Some(contact) = contact else {
        return reject(BAD_CONTACT);
    };
    let Some(announce) = announce else {
        return reject(BAD_ANNOUNCE);
    };
    if contact.doc.agent_id != announce.doc.agent_id {
        return reject(AGENT_MISMATCH);
    }
    if !announce.doc.capabilities.iter().any(|c| c == capability) {
        return reject(CAPABILITY_MISSING);
    }
    let (c, a) = (&contact.doc, &announce.doc);
    if check_validity(&c.issued_at, &c.expires_at, now_unix).is_err()
        || check_validity(&a.issued_at, &a.expires_at, now_unix).is_err()
    {
        return reject(EXPIRED);
    }
    Candidate {
        agent_id,
        valid: true,
        reason: None,
        direct_addrs: contact.doc.iroh.direct_addrs,
    }
}

/// One candidate per agent, in the order each agent first appears: its
/// first valid copy, or its first copy when none is valid. Candidates with
/// no agent are all kept.
pub fn dedupe(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut kept: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let seen = candidate
            .agent_id
            .and_then(|id| kept.iter().position(|k| k.agent_id == Some(id)));
        match seen {
            Some(i) if candidate.valid && !kept[i].valid => kept[i] = candidate,
            Some(_) => {}
            None => kept.push(candidate),
        }
    }
    kept
}

/// The agent's contact record: its Iroh endpoint, which is the agent's own
/// key, at up to [`MAX_DIRECT_ADDRS`] of `addrs`, with no relay.
pub fn issue_contact(
    key: &AgentKey,
    addrs: &[SocketAddr],
    seq: u64,
    now_unix: i64,
) -> anyhow::Result<Signed<ContactRecord>> {
    let (issued_at, expires_at) = lifetime(now_unix)?;
    let doc = ContactRecord {
        schema: CONTACT_SCHEMA.to_owned(),
        agent_id: key.id(),
        iroh: IrohContact {
            endpoint_id: key.id(),
            relay_url: None,
            direct_addrs: addrs
                .iter()
                .take(MAX_DIRECT_ADDRS)
                .map(SocketAddr::to_string)
                .collect(),
        },
        seq,
        issued_at,
        expires_at,
    };
    Signed::issue(key, doc).context("cannot issue the contact record")
}

/// The agent's announce record, listing `capabilities`.
pub fn issue_announce(
    key: &AgentKey,
    capabilities: &[String],
    seq: u64,
    now_unix: i64,
) -> anyhow::Result<Signed<AnnounceRecord>> {
    let (issued_at, expires_at) = lifetime(now_unix)?;
    let doc = AnnounceRecord {
        schema: ANNOUNCE_SCHEMA.to_owned(),
        agent_id: key.id(),
        seq,
        agent_card_hash: NO_CARD.parse::<CardHash>()?,
        capabilities: capabilities.to_vec(),
        discovery_alpn: String::from_utf8(ALPN_DISCOVERY.to_vec())?,
        issued_at,
        expires_at,
    };
    Signed::issue(key, doc).context("cannot issue the announce record")
}

/// What `--rogue` publishes as its contact: `signed` with the first byte of
/// its signature flipped, so that the signature no longer verifies while
/// the document and the envelope stay well formed.
pub fn tamper_signature<T: Clone>(signed: &Signed<T>) -> Signed<T> {
    let mut signature = URL_SAFE_NO_PAD
        .decode(&signed.signature)
        .unwrap_or_else(|_| vec![0; 64]);
    if let Some(first) = signature.first_mut() {
        *first ^= 0x01;
    }
    Signed {
        doc: signed.doc.clone(),
        signature: URL_SAFE_NO_PAD.encode(signature),
    }
}

/// The addresses a node bound to `listen` is reachable at: `listen` itself
/// when its IP is specific; for `0.0.0.0`, every local IPv4 address but
/// loopback, from the kernel's own table (`/proc/net/fib_trie`, Linux only),
/// sorted, with `listen`'s port. Read again on every announce, so that a
/// node that changed networks publishes where it is now (scenario S7).
/// Loopback when the table cannot be read or lists nothing else.
pub fn local_addrs(listen: SocketAddr) -> Vec<SocketAddr> {
    if !listen.ip().is_unspecified() {
        return vec![listen];
    }
    let table = std::fs::read_to_string("/proc/net/fib_trie").unwrap_or_default();
    let mut ips = local_ipv4s(&table);
    if ips.is_empty() {
        ips.push(Ipv4Addr::LOCALHOST);
    }
    ips.into_iter()
        .map(|ip| SocketAddr::from((ip, listen.port())))
        .collect()
}

/// The local IPv4 addresses `/proc/net/fib_trie` lists, loopback left out:
/// each `|-- <ip>` line followed by a `/32 host LOCAL` one.
fn local_ipv4s(table: &str) -> Vec<Ipv4Addr> {
    let mut ips = Vec::new();
    let mut last = None;
    for line in table.lines().map(str::trim_start) {
        if let Some(ip) = line.strip_prefix("|-- ") {
            last = ip.parse::<Ipv4Addr>().ok();
        } else if line.starts_with("/32 host LOCAL")
            && let Some(ip) = last.take()
            && !ip.is_loopback()
            && !ips.contains(&ip)
        {
            ips.push(ip);
        }
    }
    ips.sort();
    ips
}

/// `issued_at` and `expires_at` of a record issued at `now_unix`.
fn lifetime(now_unix: i64) -> anyhow::Result<(String, String)> {
    let at = |unix| format_timestamp(unix).context("the clock is out of range");
    Ok((at(now_unix)?, at(now_unix + RECORD_TTL_S)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ipv4s_reads_the_kernel_table() {
        let table = "Main:\n  +-- 0.0.0.0/0 2 0 2\n     +-- 127.0.0.0/8 2 0 2\n\
            \x20          |-- 127.0.0.1\n              /32 host LOCAL\n\
            \x20             |-- 172.27.0.0\n                 /16 link UNICAST\n\
            \x20             |-- 172.28.0.2\n                 /32 host LOCAL\n\
            \x20          |-- 172.27.255.255\n              /32 link BROADCAST\n\
            \x20             |-- 172.27.0.2\n                 /32 host LOCAL\n\
            Local:\n             |-- 172.27.0.2\n                 /32 host LOCAL\n";
        let ips: Vec<Ipv4Addr> = vec!["172.27.0.2".parse().unwrap(), "172.28.0.2".parse().unwrap()];
        assert_eq!(local_ipv4s(table), ips);
    }

    #[test]
    fn a_specific_listen_address_is_its_own() {
        let listen: SocketAddr = "192.0.2.7:4433".parse().unwrap();
        assert_eq!(local_addrs(listen), [listen]);
    }
}
