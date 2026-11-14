//! Pages of a view: where each one ends ([`page`]), the cursors that chain
//! them ([`CursorKey`]), and the signed page ([`sign_page`]).

use std::fmt;
use std::sync::LazyLock;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use super::{
    MAX_CURSOR_BYTES, MAX_PAGE_BYTES, MAX_QUERY_TOTAL_BYTES, PageLimit, SIGNATURE_CHARS,
    VIEW_SCHEMA, VIEW_TTL_S, ViewEntry, ViewError, ViewPage, normalized, page_entries, to_value,
};
use crate::card_jws::CardHash;
use crate::grant::format_timestamp;
use crate::identity::{AgentId, AgentKey};
use crate::inventory::{Scope, Section};
use crate::jcs::canonical;
use crate::signed::{Signed, SignedError};

/// The key this run's cursors are authenticated with. Never stored: a new
/// one on every start, so the cursors of a run die with it.
pub struct CursorKey([u8; 32]);

impl CursorKey {
    /// A new key from the operating system's random number generator.
    ///
    /// # Panics
    ///
    /// If the operating system cannot provide random bytes.
    pub fn random() -> Self {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("the OS random number generator failed");
        Self(key)
    }

    fn mac(&self, payload: &[u8]) -> blake3::Hash {
        blake3::keyed_hash(&self.0, payload)
    }
}

impl fmt::Debug for CursorKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CursorKey(..)")
    }
}

/// What a cursor carries: what it serves, and where its chain is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audience: Option<AgentId>,
    sections: Vec<Section>,
    revision: u64,
    /// The first entry of the next page.
    offset: u64,
    /// The bytes of the chain's pages so far.
    bytes_so_far: u64,
}

impl Cursor {
    fn encode(&self, key: &CursorKey) -> String {
        let payload = canonical(&to_value(self));
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(key.mac(&payload).as_bytes())
        )
    }

    /// The cursor in `text`, if `key` made it.
    fn decode(text: &str, key: &CursorKey) -> Option<Self> {
        if text.len() > MAX_CURSOR_BYTES {
            return None;
        }
        let (payload, mac) = text.split_once('.')?;
        let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
        let mac: [u8; 32] = URL_SAFE_NO_PAD.decode(mac).ok()?.try_into().ok()?;
        // `Hash` compares in constant time.
        if key.mac(&payload) != blake3::Hash::from(mac) {
            return None;
        }
        serde_json::from_slice(&payload).ok()
    }
}

/// A request for a page of a view.
#[derive(Debug, Clone, Copy)]
pub struct PageRequest<'a> {
    /// The requester's scope, from its grant.
    pub scope: &'a Scope,
    /// The requester, `None` in the public scope.
    pub audience: Option<&'a AgentId>,
    /// The sections asked for, in order and without repeats, as the page
    /// carries them.
    pub sections: &'a [Section],
    /// The previous page's `next_cursor`; `None` for the first page.
    pub cursor: Option<&'a str>,
    /// The most entries the requester wants: at most
    /// [`MAX_PAGE_ENTRIES`](super::MAX_PAGE_ENTRIES), which `None` and
    /// `Some(0)` also mean.
    pub page_entries: Option<u32>,
    /// The scope's revision, which the page carries as `seq`.
    pub revision: u64,
}

/// The entries of a page, its `next_cursor` and its `truncated_by`.
pub type PageSlice = (Vec<ViewEntry>, Option<String>, Option<PageLimit>);

/// The page of `entries`, the view `req` asks for, that starts at its
/// cursor: the entries, the cursor of the next page and the limit that ended
/// it ([`PageLimit`]).
///
/// The page is sized for a [`ViewPage`] of `req` (its scope, audience,
/// sections and revision as `seq`), signed: with the entries, it is at most
/// [`MAX_PAGE_BYTES`], and the bytes of the chain's pages are at most
/// [`MAX_QUERY_TOTAL_BYTES`]. When the next page could not hold even its
/// first entry within that total, this one is the last, with
/// [`PageLimit::QueryTotal`].
///
/// [`ViewError::InvalidCursor`] when the cursor is not one `key` made for
/// this scope, audience, sections and revision, whatever is wrong with it;
/// [`ViewError::TooLarge`] when an entry does not fit in a page on its own.
pub fn page(
    entries: &[ViewEntry],
    req: &PageRequest<'_>,
    key: &CursorKey,
) -> Result<PageSlice, ViewError> {
    let binding = Cursor {
        scope: req.scope.key(),
        audience: req.audience.copied(),
        sections: normalized(req.sections),
        revision: req.revision,
        offset: 0,
        bytes_so_far: 0,
    };
    let (offset, bytes_so_far) = match req.cursor {
        None => (0, 0),
        Some(text) => {
            let cursor = Cursor::decode(text, key).ok_or(ViewError::InvalidCursor)?;
            let offset = usize::try_from(cursor.offset)
                .ok()
                .filter(|&offset| offset <= entries.len());
            let bytes_so_far = usize::try_from(cursor.bytes_so_far)
                .ok()
                .filter(|&bytes| bytes <= MAX_QUERY_TOTAL_BYTES);
            let serves = cursor
                == Cursor {
                    offset: cursor.offset,
                    bytes_so_far: cursor.bytes_so_far,
                    ..binding.clone()
                };
            match (serves, offset, bytes_so_far) {
                (true, Some(offset), Some(bytes_so_far)) => (offset, bytes_so_far),
                _ => return Err(ViewError::InvalidCursor),
            }
        }
    };

    let max_entries = page_entries(req.page_entries);
    let overhead = page_overhead(req, &binding, entries.len(), key);
    let entry_bytes = |entry: &ViewEntry| canonical(&to_value(entry)).len();
    let mut size = overhead;
    let mut end = offset;
    let mut limit = None;
    while end < entries.len() {
        if end - offset == max_entries {
            limit = Some(PageLimit::PageEntries);
            break;
        }
        // A comma before every entry but the first.
        let bytes = entry_bytes(&entries[end]) + usize::from(end > offset);
        if end == offset && overhead + bytes > MAX_PAGE_BYTES {
            return Err(ViewError::TooLarge);
        }
        if bytes_so_far + size + bytes > MAX_QUERY_TOTAL_BYTES {
            limit = Some(PageLimit::QueryTotal);
            break;
        }
        if size + bytes > MAX_PAGE_BYTES {
            limit = Some(PageLimit::PageBytes);
            break;
        }
        size += bytes;
        end += 1;
    }

    let mut next_cursor = None;
    if matches!(limit, Some(PageLimit::PageBytes | PageLimit::PageEntries)) {
        let bytes_so_far = bytes_so_far + size;
        if bytes_so_far + overhead + entry_bytes(&entries[end]) > MAX_QUERY_TOTAL_BYTES {
            limit = Some(PageLimit::QueryTotal);
        } else {
            let next = Cursor {
                offset: end as u64,
                bytes_so_far: bytes_so_far as u64,
                ..binding
            };
            next_cursor = Some(next.encode(key));
        }
    }
    Ok((entries[offset..end].to_vec(), next_cursor, limit))
}

/// The page `slice` of the view `req` asks for, as `page` cut it, signed by
/// `key` at `now_unix` for [`VIEW_TTL_S`], naming the card of `card_hash`:
/// the page `page` sized, with `req`'s scope and audience, its revision as
/// `seq` and its sections in order without repeats.
///
/// [`ViewError::TooLarge`] when the signed page would exceed
/// [`MAX_PAGE_BYTES`], which a slice from [`page`] never does;
/// [`ViewError::Malformed`] when it would not be a valid page: a time
/// outside the years 0000 to 9999, or a scope whose project is not a
/// project name.
pub fn sign_page(
    key: &AgentKey,
    card_hash: &CardHash,
    req: &PageRequest<'_>,
    slice: PageSlice,
    now_unix: i64,
) -> Result<Signed<ViewPage>, ViewError> {
    let (entries, next_cursor, truncated_by) = slice;
    // Twenty characters each, as `page` sized them.
    let issued_at = format_timestamp(now_unix).ok_or(ViewError::Malformed)?;
    let expires_at = now_unix
        .checked_add(VIEW_TTL_S)
        .and_then(format_timestamp)
        .ok_or(ViewError::Malformed)?;
    let doc = ViewPage {
        schema: VIEW_SCHEMA.to_owned(),
        agent_id: key.id(),
        scope: req.scope.key(),
        audience: req.audience.copied(),
        seq: req.revision,
        issued_at,
        expires_at,
        agent_card_hash: card_hash.clone(),
        sections: normalized(req.sections),
        entries,
        next_cursor,
        truncated_by,
    };
    Signed::issue(key, doc).map_err(|err| match err {
        SignedError::TooLarge => ViewError::TooLarge,
        _ => ViewError::Malformed,
    })
}

/// The bytes of a signed page of `req` without entries, at least: every
/// field at its longest, a cursor with the largest numbers its chain of
/// `entries` can reach and the longest limit. The entries add their JCS and
/// a comma between each two. [`sign_page`] builds the page with these
/// fields, and no other.
fn page_overhead(
    req: &PageRequest<'_>,
    binding: &Cursor,
    entries: usize,
    key: &CursorKey,
) -> usize {
    /// Any agent id: every one has 64 characters.
    static ANY_AGENT: LazyLock<AgentId> = LazyLock::new(|| AgentKey::from_bytes(&[0; 32]).id());
    /// Every timestamp has 20 characters, and every card hash 71.
    const ANY_TIMESTAMP: &str = "2026-11-14T00:00:00Z";
    const ANY_CARD_HASH: &str =
        "blake3:0000000000000000000000000000000000000000000000000000000000000000";

    let longest_cursor = Cursor {
        offset: entries as u64,
        bytes_so_far: MAX_QUERY_TOTAL_BYTES as u64,
        ..binding.clone()
    };
    let skeleton = ViewPage {
        schema: VIEW_SCHEMA.to_owned(),
        agent_id: *ANY_AGENT,
        scope: binding.scope.clone(),
        audience: req.audience.copied(),
        seq: req.revision,
        issued_at: ANY_TIMESTAMP.to_owned(),
        expires_at: ANY_TIMESTAMP.to_owned(),
        agent_card_hash: ANY_CARD_HASH.parse().expect("a card hash"),
        sections: binding.sections.clone(),
        entries: Vec::new(),
        next_cursor: Some(longest_cursor.encode(key)),
        truncated_by: Some(PageLimit::PageEntries),
    };
    Signed {
        doc: skeleton,
        signature: "A".repeat(SIGNATURE_CHARS),
    }
    .to_json()
    .len()
}
