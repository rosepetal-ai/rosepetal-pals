//! Shared code of the hito 3 backend spikes, which is not product code.
//!
//! Task 2 of the plan writes the node contract here. Until then the crate
//! only links `pals-core`, whose records the candidates carry unchanged.

/// The schemas of the records every candidate carries, from `pals-core`.
pub const RECORD_SCHEMAS: [&str; 2] = [
    pals_core::records::CONTACT_SCHEMA,
    pals_core::records::ANNOUNCE_SCHEMA,
];
