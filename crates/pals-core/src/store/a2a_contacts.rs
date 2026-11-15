//! Plain A2A contacts: agents that speak A2A with no PALS daemon behind
//! them, reached at the URL the operator gave, saved under a local name
//! (H2-D3, outbound only).
//!
//! **One name, one contact.** A plain contact cannot take a PALS contact's
//! name, nor the reverse ([`Store::contact_put`]), and its name is never an
//! agent id, so that `{peer}` resolves without ambiguity
//! ([`Store::resolve_peer`]): an agent id first, then a PALS contact's name,
//! then a plain contact's. The name is also a path segment of the gateway's
//! URLs (`/peers/<name>`): [`is_plain_contact_name`] says which are.
//!
//! **What is kept** is what the operator gave, checked by the caller: the
//! card's URL, its origin (the agent's identity, and the only origin ever
//! called), and the name of the environment variable that holds its bearer
//! token, if it needs one; never the token.
//!
//! **Peer references.** What the outbox keeps of the peer a question went
//! to, and what `peer_cards` keys a card by, is [`PeerRef::key`]: the agent
//! id in hex, or `a2a:<name>` ([`PLAIN_PEER_PREFIX`]); [`Store::peer_ref`]
//! reads it back.

use anyhow::ensure;
use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};

use super::Store;
use crate::identity::AgentId;

/// The prefix of a plain contact's [`PeerRef::key`]: `a2a:<name>`.
pub const PLAIN_PEER_PREFIX: &str = "a2a:";
/// The longest name of a plain contact, in bytes.
pub const MAX_PLAIN_CONTACT_NAME: usize = 64;

/// A plain A2A agent saved under a local name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A2aContact {
    /// Unique among contacts, PALS and plain ones; never an agent id.
    pub name: String,
    /// Where its Agent Card is read.
    pub card_url: String,
    /// The origin of `card_url`, `https://<host>[:<port>]`: the only one
    /// called.
    pub origin: String,
    /// The environment variable whose value is sent as its bearer token.
    pub bearer_env: Option<String>,
    /// Unix seconds.
    pub added_at: i64,
}

/// The agent a `{peer}` names, at the gateway and in the outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerRef {
    /// A PALS peer, by its agent id.
    Pals(AgentId),
    /// A plain A2A contact.
    Plain(A2aContact),
}

impl PeerRef {
    /// What the outbox's `peer` column and `peer_cards` keep of it: the
    /// agent id in hex, or `a2a:<name>`.
    pub fn key(&self) -> String {
        match self {
            Self::Pals(id) => id.to_string(),
            Self::Plain(contact) => format!("{PLAIN_PEER_PREFIX}{}", contact.name),
        }
    }
}

/// Whether `name` can be a plain contact's: 1 to
/// [`MAX_PLAIN_CONTACT_NAME`] ASCII letters, digits, `-`, `_` or `.`, not
/// starting with `.` (so never a dot segment), and not an agent id.
pub fn is_plain_contact_name(name: &str) -> bool {
    (1..=MAX_PLAIN_CONTACT_NAME).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && name.parse::<AgentId>().is_err()
}

const COLUMNS: &str = "name, card_url, origin, bearer_env, added_at";

impl A2aContact {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            name: row.get(0)?,
            card_url: row.get(1)?,
            origin: row.get(2)?,
            bearer_env: row.get(3)?,
            added_at: row.get(4)?,
        })
    }
}

impl Store {
    /// Saves `c`, replacing the plain contact with the same name. Fails
    /// when its name is an agent id, is not a [plain contact
    /// name](is_plain_contact_name), or is a PALS contact's.
    pub fn a2a_contact_put(&self, c: &A2aContact) -> anyhow::Result<()> {
        ensure!(
            c.name.parse::<AgentId>().is_err(),
            "a contact name cannot be an agent id: peers are reachable by their id already"
        );
        ensure!(
            is_plain_contact_name(&c.name),
            "{:?} cannot be a plain contact's name: 1 to {MAX_PLAIN_CONTACT_NAME} letters, \
             digits, '-', '_' or '.', not starting with '.'",
            c.name
        );
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let taken: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM contacts WHERE name = ?1)",
            [&c.name],
            |row| row.get(0),
        )?;
        ensure!(
            !taken,
            "{} is the name of a PALS contact already: one name, one contact",
            c.name
        );
        tx.execute(
            &format!(
                "INSERT INTO a2a_contacts ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (name) DO UPDATE SET
                     card_url = excluded.card_url,
                     origin = excluded.origin,
                     bearer_env = excluded.bearer_env,
                     added_at = excluded.added_at"
            ),
            params![c.name, c.card_url, c.origin, c.bearer_env, c.added_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn a2a_contact_by_name(&self, name: &str) -> anyhow::Result<Option<A2aContact>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM a2a_contacts WHERE name = ?1"),
                [name],
                A2aContact::from_row,
            )
            .optional()?)
    }

    /// Every plain contact, by name.
    pub fn a2a_contacts(&self) -> anyhow::Result<Vec<A2aContact>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare(&format!("SELECT {COLUMNS} FROM a2a_contacts ORDER BY name"))?;
        let contacts = stmt
            .query_map([], A2aContact::from_row)?
            .collect::<Result<_, _>>()?;
        Ok(contacts)
    }

    /// The agent `peer` names: an agent id, else the PALS contact of that
    /// name, else the plain contact of that name; `None` when it names no
    /// one.
    pub fn resolve_peer(&self, peer: &str) -> anyhow::Result<Option<PeerRef>> {
        if let Ok(id) = peer.parse::<AgentId>() {
            return Ok(Some(PeerRef::Pals(id)));
        }
        if let Some(contact) = self.contact_by_name(peer)? {
            return Ok(Some(PeerRef::Pals(contact.agent_id)));
        }
        Ok(self.a2a_contact_by_name(peer)?.map(PeerRef::Plain))
    }

    /// The agent a [`PeerRef::key`] names: `None` when it is a plain
    /// contact no longer saved, or not a key.
    pub fn peer_ref(&self, key: &str) -> anyhow::Result<Option<PeerRef>> {
        if let Some(name) = key.strip_prefix(PLAIN_PEER_PREFIX) {
            return Ok(self.a2a_contact_by_name(name)?.map(PeerRef::Plain));
        }
        Ok(key.parse::<AgentId>().ok().map(PeerRef::Pals))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentKey;
    use crate::store::Contact;

    fn plain(name: &str) -> A2aContact {
        A2aContact {
            name: name.to_owned(),
            card_url: "https://agent.example/.well-known/agent-card.json".to_owned(),
            origin: "https://agent.example".to_owned(),
            bearer_env: Some("AGENT_TOKEN".to_owned()),
            added_at: 10,
        }
    }

    #[test]
    fn plain_contacts_are_kept_by_name_and_read_back_by_their_key() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        store.a2a_contact_put(&plain("zeta")).unwrap();
        store.a2a_contact_put(&plain("alpha")).unwrap();
        let moved = A2aContact {
            card_url: "https://moved.example/card.json".to_owned(),
            origin: "https://moved.example".to_owned(),
            bearer_env: None,
            ..plain("zeta")
        };
        store.a2a_contact_put(&moved).unwrap();
        assert_eq!(
            store.a2a_contacts().unwrap(),
            [plain("alpha"), moved.clone()]
        );
        assert_eq!(store.a2a_contact_by_name("nobody").unwrap(), None);

        let reference = PeerRef::Plain(moved.clone());
        assert_eq!(reference.key(), "a2a:zeta");
        assert_eq!(store.peer_ref("a2a:zeta").unwrap(), Some(reference));
        assert_eq!(store.peer_ref("a2a:nobody").unwrap(), None);
        let id = AgentKey::generate().id();
        assert_eq!(
            store.peer_ref(&id.to_string()).unwrap(),
            Some(PeerRef::Pals(id))
        );
        assert_eq!(store.peer_ref("zeta").unwrap(), None);
    }

    #[test]
    fn a_name_is_one_contact_s() {
        let store = Store::open_in_memory(AgentKey::generate().id()).unwrap();
        let id = AgentKey::generate().id();
        let pals = |name: &str| Contact {
            name: name.to_owned(),
            agent_id: id,
            ticket: None,
            added_at: 1,
        };
        store.contact_put(&pals("bob")).unwrap();
        assert!(store.a2a_contact_put(&plain("bob")).is_err());
        store.a2a_contact_put(&plain("echo")).unwrap();
        assert!(store.contact_put(&pals("echo")).is_err());
        // Each kind still replaces its own.
        store.contact_put(&pals("bob")).unwrap();
        store.a2a_contact_put(&plain("echo")).unwrap();

        assert_eq!(store.resolve_peer("bob").unwrap(), Some(PeerRef::Pals(id)));
        assert_eq!(
            store.resolve_peer("echo").unwrap(),
            Some(PeerRef::Plain(plain("echo")))
        );
        assert_eq!(
            store.resolve_peer(&id.to_string()).unwrap(),
            Some(PeerRef::Pals(id))
        );
        assert_eq!(store.resolve_peer("nobody").unwrap(), None);
    }

    #[test]
    fn plain_contact_names_are_plain_path_segments() {
        for name in ["echo", "a-b_c.d", "A1", &"x".repeat(64)] {
            assert!(is_plain_contact_name(name), "{name}");
        }
        let id = AgentKey::generate().id().to_string();
        let long = "x".repeat(65);
        for name in [
            "", ".", "..", ".hidden", "a/b", "a b", "a2a:x", "ü", "a%2F", &id, &long,
        ] {
            assert!(!is_plain_contact_name(name), "{name:?}");
        }
    }
}
