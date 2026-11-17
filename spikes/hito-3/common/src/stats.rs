//! What a node reports on `GET /stats`: its backend's [`BackendStats`] and
//! its resident memory.

use std::collections::BTreeMap;

use pals_core::identity::AgentId;
use serde::Serialize;

/// A backend's counters, as they are now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BackendStats {
    /// Connections open now, to any peer.
    pub connections: u64,
    /// Peers in the routing table (for the baseline, its contacts).
    pub routing_peers: u64,
    /// Records this node stores for others: values such as the signed
    /// contact and announce records.
    pub records_stored: u64,
    /// Provider records this node stores for others: pointers from a
    /// capability to an agent that offers it.
    pub provider_records_stored: u64,
    /// Records offered to this node for storage that it refused because
    /// they did not verify, since it started (scenario S5).
    pub records_rejected: u64,
    /// How another node names this one in `--bootstrap`, in the backend's
    /// own form; `None` when it cannot be a bootstrap node.
    pub bootstrap_addr: Option<String>,
    /// What this node stores about each agent, the agents it stores nothing
    /// about left out: the router-side checks of scenarios S3 (a stopped
    /// provider's records go away) and S5 (an invalid contact is never
    /// stored). A node that stores records must fill it, or S5 ends in
    /// `error`: it cannot tell whose the records are.
    pub stored_by_agent: BTreeMap<AgentId, StoredRecords>,
}

/// What a node stores about one agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StoredRecords {
    /// Its contact records, counted in [`BackendStats::records_stored`].
    pub contact_records: u64,
    /// Its announce records, counted in [`BackendStats::records_stored`].
    pub announce_records: u64,
    /// Any other record about it counted in [`BackendStats::records_stored`]
    /// (an identity binding, say): S5 counts as the rogue's contact any
    /// growth of `records_stored` that `stored_by_agent` does not explain.
    pub other_records: u64,
    /// Provider records that point to it, counted in
    /// [`BackendStats::provider_records_stored`].
    pub provider_records: u64,
}

/// This process's resident set size in KiB, `VmRSS` in `/proc/self/status`;
/// `None` where there is no such file.
pub fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    vm_rss_kib(&status)
}

fn vm_rss_kib(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let mut fields = line["VmRSS:".len()..].split_whitespace();
    let kib = fields.next()?.parse().ok()?;
    (fields.next() == Some("kB")).then_some(kib)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_rss_is_read_in_kib() {
        let status = "Name:\tspike\nVmHWM:\t   9000 kB\nVmRSS:\t   8124 kB\nThreads:\t4\n";
        assert_eq!(vm_rss_kib(status), Some(8124));
        assert_eq!(vm_rss_kib("Name:\tspike\n"), None);
    }
}
