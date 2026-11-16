//! Blob URLs across the gateway.
//!
//! Between peers a file part's `url` is `iroh-blob://<holder>/<hash hex>`; a
//! stock A2A client cannot fetch that, so the gateway shows it as
//! `<gateway>/blobs/<holder>/<hash hex>`, which it serves. [`to_gateway`]
//! rewrites what comes from a peer, [`to_wire`] what goes to one; each undoes
//! the other.
//!
//! Only the value of a `url` member is rewritten, wherever it is in the
//! document, and only when it is exactly a blob URL of the other form: a
//! text that mentions a URL, or a URL that is not a PALS blob, stays as it
//! is.

use iroh_blobs::Hash;
use pals_core::identity::AgentId;
use serde_json::{Map, Value};

use crate::blobs::{BlobRef, parse_holder_and_hash, wire_url};

/// The member that holds a file part's URL.
const URL: &str = "url";

/// Rewrites every `iroh-blob://<holder>/<hash>` URL to
/// `<base_url>/blobs/<holder>/<hash>`.
pub fn to_gateway(value: &mut Value, base_url: &str) {
    rewrite_to_gateway(value, base_url);
}

/// Rewrites every `<base_url>/blobs/<holder>/<hash>` URL to
/// `iroh-blob://<holder>/<hash>`.
pub fn to_wire(value: &mut Value, base_url: &str) {
    rewrite_to_wire(value, base_url);
}

/// [`to_gateway`]; whether it changed anything.
pub(crate) fn rewrite_to_gateway(value: &mut Value, base_url: &str) -> bool {
    rewrite_urls(value, &mut |url| {
        let (holder, hash) = BlobRef::parse_url(url)?;
        Some(gateway_url(base_url, &holder, &hash))
    })
}

/// [`to_wire`]; whether it changed anything.
pub(crate) fn rewrite_to_wire(value: &mut Value, base_url: &str) -> bool {
    rewrite_urls(value, &mut |url| {
        let (holder, hash) = gateway_blob(url, base_url)?;
        Some(wire_url(&holder, &hash))
    })
}

/// The holder and hash of `url` when it is exactly a blob URL of the
/// gateway at `base_url`: `<base_url>/blobs/<holder>/<hash hex>`.
pub(crate) fn gateway_blob(url: &str, base_url: &str) -> Option<(AgentId, Hash)> {
    let path = url.strip_prefix(base_url)?.strip_prefix("/blobs/")?;
    parse_holder_and_hash(path)
}

/// `<base_url>/blobs/<holder>/<hash hex>`, where the gateway serves a blob.
pub(crate) fn gateway_url(base_url: &str, holder: &AgentId, hash: &Hash) -> String {
    format!("{base_url}/blobs/{holder}/{}", hash.to_hex())
}

/// Calls `visit` with every object of `value` whose `url` is a blob URL
/// between peers, and that URL's holder and hash.
pub(crate) fn for_each_blob_part(
    value: &mut Value,
    visit: &mut impl FnMut(&mut Map<String, Value>, AgentId, Hash),
) {
    match value {
        Value::Object(object) => {
            let blob = object
                .get(URL)
                .and_then(Value::as_str)
                .and_then(BlobRef::parse_url);
            if let Some((holder, hash)) = blob {
                visit(object, holder, hash);
            }
            for (name, member) in object.iter_mut() {
                if name != URL {
                    for_each_blob_part(member, visit);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                for_each_blob_part(item, visit);
            }
        }
        _ => {}
    }
}

/// Replaces each string `url` member of `value` for which `rewrite` has a
/// replacement; whether it replaced any.
fn rewrite_urls(value: &mut Value, rewrite: &mut impl FnMut(&str) -> Option<String>) -> bool {
    let mut changed = false;
    match value {
        Value::Object(object) => {
            for (name, member) in object.iter_mut() {
                if name == URL
                    && let Value::String(url) = member
                {
                    if let Some(replacement) = rewrite(url) {
                        *url = replacement;
                        changed = true;
                    }
                    continue;
                }
                changed |= rewrite_urls(member, rewrite);
            }
        }
        Value::Array(items) => {
            for item in items {
                changed |= rewrite_urls(item, rewrite);
            }
        }
        _ => {}
    }
    changed
}
