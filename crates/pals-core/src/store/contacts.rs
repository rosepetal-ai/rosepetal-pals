//! Saved peers, by a local name.

use rusqlite::{OptionalExtension, Row, params};

use super::Store;
use crate::identity::AgentId;

/// A peer saved under a local name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    /// Unique among contacts.
    pub name: String,
    pub agent_id: AgentId,
    /// An Iroh endpoint ticket to dial the peer by, if one is known.
    pub ticket: Option<String>,
    /// Unix seconds.
    pub added_at: i64,
}

const COLUMNS: &str = "name, agent_id, ticket, added_at";

impl Contact {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            name: row.get(0)?,
            agent_id: row.get(1)?,
            ticket: row.get(2)?,
            added_at: row.get(3)?,
        })
    }
}

impl Store {
    /// Saves `c`, replacing the contact with the same name.
    pub fn contact_put(&self, c: &Contact) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO contacts (name, agent_id, ticket, added_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (name) DO UPDATE SET
                 agent_id = excluded.agent_id,
                 ticket = excluded.ticket,
                 added_at = excluded.added_at",
            params![c.name, c.agent_id, c.ticket, c.added_at],
        )?;
        Ok(())
    }

    pub fn contact_by_name(&self, name: &str) -> anyhow::Result<Option<Contact>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM contacts WHERE name = ?1"),
                [name],
                Contact::from_row,
            )
            .optional()?)
    }

    /// The contact for `id`; the first by name when several share it.
    pub fn contact_by_id(&self, id: &AgentId) -> anyhow::Result<Option<Contact>> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM contacts WHERE agent_id = ?1 ORDER BY name LIMIT 1"
                ),
                [id],
                Contact::from_row,
            )
            .optional()?)
    }

    /// Every contact, by name.
    pub fn contacts(&self) -> anyhow::Result<Vec<Contact>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM contacts ORDER BY name"))?;
        let contacts = stmt
            .query_map([], Contact::from_row)?
            .collect::<Result<_, _>>()?;
        Ok(contacts)
    }
}
