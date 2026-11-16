//! The cache of signed pages ([`ViewCache`]).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{MAX_CACHED_PAGES, PageRequest, normalized, page_entries};
use crate::identity::AgentId;
use crate::inventory::Section;

/// Signed pages this agent made, kept for the requests that would make them
/// again: by scope, requester, sections, cursor, page size and revision, so
/// a page made for one requester never serves another. At most
/// [`MAX_CACHED_PAGES`]: one more drops the one that goes out soonest.
/// `Send + Sync`.
pub struct ViewCache {
    pages: Mutex<BTreeMap<CacheKey, Cached>>,
}

/// Everything that makes two pages differ.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    scope: String,
    audience: Option<AgentId>,
    sections: Vec<Section>,
    cursor: Option<String>,
    page_entries: usize,
    revision: u64,
}

impl CacheKey {
    fn of(req: &PageRequest<'_>) -> Self {
        Self {
            scope: req.scope.key(),
            audience: req.audience.copied(),
            sections: normalized(req.sections),
            cursor: req.cursor.map(str::to_owned),
            page_entries: page_entries(req.page_entries),
            revision: req.revision,
        }
    }
}

struct Cached {
    page: Vec<u8>,
    until: i64,
}

impl ViewCache {
    pub fn new() -> Self {
        Self {
            pages: Mutex::new(BTreeMap::new()),
        }
    }

    /// The page kept for `req`, while `now_unix` is before the time it was
    /// kept until.
    pub fn get(&self, req: &PageRequest<'_>, now_unix: i64) -> Option<Vec<u8>> {
        let key = CacheKey::of(req);
        let mut pages = self.pages();
        match pages.get(&key) {
            Some(cached) if now_unix < cached.until => Some(cached.page.clone()),
            Some(_) => {
                pages.remove(&key);
                None
            }
            None => None,
        }
    }

    /// Keeps `page`, the signed page made for `req`, until `until_unix`.
    pub fn put(&self, req: &PageRequest<'_>, page: Vec<u8>, until_unix: i64) {
        let key = CacheKey::of(req);
        let mut pages = self.pages();
        if !pages.contains_key(&key) && pages.len() >= MAX_CACHED_PAGES {
            let soonest = pages
                .iter()
                .min_by_key(|(_, cached)| cached.until)
                .map(|(key, _)| key.clone());
            if let Some(soonest) = soonest {
                pages.remove(&soonest);
            }
        }
        pages.insert(
            key,
            Cached {
                page,
                until: until_unix,
            },
        );
    }

    /// The pages kept.
    pub fn len(&self) -> usize {
        self.pages().len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages().is_empty()
    }

    /// A panic while the lock was held leaves a map, maybe without the page
    /// being put: taken as is.
    fn pages(&self) -> MutexGuard<'_, BTreeMap<CacheKey, Cached>> {
        self.pages.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for ViewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ViewCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ViewCache")
            .field("pages", &self.len())
            .finish()
    }
}
