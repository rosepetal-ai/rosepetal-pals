//! The KRPC messages (BEP 5, BEP 44 and n0-mainline's signed peers) a node
//! reads before n0-mainline does, and the queries a router sends itself.
//!
//! n0-mainline 0.7.2 offers a router two hooks, and neither reads a put for
//! it: `ServerSettings::filter` gets a `RequestSpecific` whose variants the
//! crate does not export, so a filter cannot match a put or read its
//! value, and `DatagramHook` gets each datagram as bytes, before the
//! library decodes it. This module reads those bytes, with
//! `serde_bencode`, which n0-mainline uses itself, as far as the hooks need,
//! and writes the few messages they send.
//!
//! The router's hook fails closed: a `put` or an `announce_signed_peer`
//! whose arguments it cannot read is refused, never let through. Bytes this
//! module cannot decode at all the library cannot decode either, since
//! both go through `serde_bencode::from_bytes`, and drops them. What the
//! hook lets through, the library decodes and checks again on its own.

use std::collections::HashMap;

use n0_mainline::Id;
use serde_bencode::value::Value;

/// A bencoded dictionary.
pub type Dict = HashMap<Vec<u8>, Value>;

/// The bytes of a signed peer in a `get_signed_peers` answer: key (32),
/// timestamp (8) and signature (64).
const SIGNED_PEER_BYTES: usize = 104;

/// A KRPC message, as far as the hooks care.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `y: q`: the method (`q`) and its arguments (`a`).
    Query {
        tid: Vec<u8>,
        method: Vec<u8>,
        args: Dict,
    },
    /// `y: r`: the answer's body (`r`).
    Response { tid: Vec<u8>, body: Dict },
    /// An error, or anything else that decodes.
    Other,
}

/// The arguments of a BEP 44 `put` of a mutable item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutablePut {
    pub key: [u8; 32],
    pub salt: Option<Vec<u8>>,
    pub value: Vec<u8>,
    pub seq: i64,
}

/// The arguments of an `announce_signed_peer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedPeer {
    pub info_hash: Id,
    pub key: [u8; 32],
}

/// The message in `bytes`; `None` when they are not a bencoded dictionary
/// with a transaction id. A query with no method, or no arguments, has them
/// empty.
pub fn parse(bytes: &[u8]) -> Option<Message> {
    let Ok(Value::Dict(mut message)) = serde_bencode::from_bytes::<Value>(bytes) else {
        return None;
    };
    let tid = take_bytes(&mut message, b"t")?;
    Some(match take_bytes(&mut message, b"y").as_deref() {
        Some(b"q") => Message::Query {
            tid,
            method: take_bytes(&mut message, b"q").unwrap_or_default(),
            args: take_dict(&mut message, b"a").unwrap_or_default(),
        },
        Some(b"r") => Message::Response {
            tid,
            body: take_dict(&mut message, b"r")?,
        },
        _ => Message::Other,
    })
}

/// The mutable item a `put`'s arguments carry; `None` when they carry an
/// immutable one (no `k`), or a malformed one, which the library refuses.
pub fn mutable_put(args: &Dict) -> Option<MutablePut> {
    Some(MutablePut {
        key: fixed(args, b"k")?,
        salt: bytes(args, b"salt").map(<[u8]>::to_vec),
        value: bytes(args, b"v")?.to_vec(),
        seq: int(args, b"seq")?,
    })
}

/// Whether a `put`'s arguments carry an immutable item: a value and no key.
pub fn is_immutable_put(args: &Dict) -> bool {
    !args.contains_key(b"k".as_slice()) && args.contains_key(b"v".as_slice())
}

/// The target a `put`'s arguments name, if it is 20 bytes.
pub fn put_target(args: &Dict) -> Option<Id> {
    fixed::<20>(args, b"target").map(Id::from)
}

/// The signed peer an `announce_signed_peer`'s arguments carry; `None`
/// when malformed, which the library refuses.
pub fn signed_peer(args: &Dict) -> Option<SignedPeer> {
    Some(SignedPeer {
        info_hash: Id::from(fixed::<20>(args, b"info_hash")?),
        key: fixed(args, b"k")?,
    })
}

/// A KRPC error that answers the query `tid`.
pub fn error(tid: &[u8], code: i64, message: &str) -> Vec<u8> {
    encode(vec![
        (b"t", Value::Bytes(tid.to_vec())),
        (b"y", Value::Bytes(b"e".to_vec())),
        (
            b"e",
            Value::List(vec![
                Value::Int(code),
                Value::Bytes(message.as_bytes().to_vec()),
            ]),
        ),
    ])
}

/// A read-only `get` of `target` (BEP 44), from the node `id`: what the
/// storing node answers with the item it holds there, if any.
pub fn get(tid: &[u8], id: &Id, target: &Id) -> Vec<u8> {
    query(tid, b"get", id, (b"target", target))
}

/// A read-only `get_signed_peers` of `info_hash`, from the node `id`.
pub fn get_signed_peers(tid: &[u8], id: &Id, info_hash: &Id) -> Vec<u8> {
    query(tid, b"get_signed_peers", id, (b"info_hash", info_hash))
}

/// The item a `get` answer carries: `None` when it carries none, and
/// otherwise its key, `None` for an immutable item (or a key that is not
/// 32 bytes).
pub fn item_in(body: &Dict) -> Option<Option<[u8; 32]>> {
    body.contains_key(b"v".as_slice())
        .then(|| fixed(body, b"k"))
}

/// The keys of the signed peers a `get_signed_peers` answer carries.
pub fn peer_keys(body: &Dict) -> Vec<[u8; 32]> {
    let Some(Value::List(peers)) = body.get(b"peers".as_slice()) else {
        return Vec::new();
    };
    peers
        .iter()
        .filter_map(|peer| match peer {
            Value::Bytes(bytes) if bytes.len() == SIGNED_PEER_BYTES => bytes[..32].try_into().ok(),
            _ => None,
        })
        .collect()
}

fn query(tid: &[u8], method: &[u8], id: &Id, (name, value): (&[u8], &Id)) -> Vec<u8> {
    let args: Dict = [
        (b"id".to_vec(), Value::Bytes(id.as_bytes().to_vec())),
        (name.to_vec(), Value::Bytes(value.as_bytes().to_vec())),
    ]
    .into();
    encode(vec![
        (b"t", Value::Bytes(tid.to_vec())),
        (b"y", Value::Bytes(b"q".to_vec())),
        (b"q", Value::Bytes(method.to_vec())),
        (b"a", Value::Dict(args)),
        // BEP 43: a read-only node, which the other side does not add to
        // its routing table.
        (b"ro", Value::Int(1)),
    ])
}

fn encode(fields: Vec<(&[u8], Value)>) -> Vec<u8> {
    let message: Dict = fields
        .into_iter()
        .map(|(name, value)| (name.to_vec(), value))
        .collect();
    // A dictionary of byte strings, integers and lists always encodes.
    serde_bencode::to_bytes(&Value::Dict(message)).unwrap_or_default()
}

fn take_bytes(dict: &mut Dict, name: &[u8]) -> Option<Vec<u8>> {
    match dict.remove(name) {
        Some(Value::Bytes(bytes)) => Some(bytes),
        _ => None,
    }
}

fn take_dict(dict: &mut Dict, name: &[u8]) -> Option<Dict> {
    match dict.remove(name) {
        Some(Value::Dict(inner)) => Some(inner),
        _ => None,
    }
}

fn bytes<'a>(dict: &'a Dict, name: &[u8]) -> Option<&'a [u8]> {
    match dict.get(name) {
        Some(Value::Bytes(bytes)) => Some(bytes),
        _ => None,
    }
}

fn fixed<const N: usize>(dict: &Dict, name: &[u8]) -> Option<[u8; N]> {
    bytes(dict, name)?.try_into().ok()
}

fn int(dict: &Dict, name: &[u8]) -> Option<i64> {
    match dict.get(name) {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_reads_back() {
        let (id, target) = (Id::from([1; 20]), Id::from([2; 20]));
        let message = parse(&get(b"pr01", &id, &target)).unwrap();
        let Message::Query { tid, method, args } = message else {
            panic!("not a query: {message:?}");
        };
        assert_eq!(
            (tid.as_slice(), method.as_slice()),
            (&b"pr01"[..], &b"get"[..])
        );
        assert_eq!(bytes(&args, b"target"), Some(target.as_bytes().as_slice()));
        assert!(parse(b"not bencode").is_none());
    }

    #[test]
    fn a_mutable_put_is_read_and_an_immutable_one_told_apart() {
        let args: Dict = [
            (b"k".to_vec(), Value::Bytes(vec![7; 32])),
            (b"salt".to_vec(), Value::Bytes(b"s".to_vec())),
            (b"v".to_vec(), Value::Bytes(b"{}".to_vec())),
            (b"seq".to_vec(), Value::Int(9)),
        ]
        .into();
        let put = mutable_put(&args).unwrap();
        assert_eq!((put.key, put.seq), ([7; 32], 9));
        assert_eq!(put.salt.as_deref(), Some(&b"s"[..]));
        assert!(!is_immutable_put(&args));
        let immutable: Dict = [(b"v".to_vec(), Value::Bytes(b"x".to_vec()))].into();
        assert!(mutable_put(&immutable).is_none());
        assert!(is_immutable_put(&immutable));
    }

    #[test]
    fn an_error_answers_its_query() {
        let bytes = error(b"ab", 203, "agentnet: bad_contact");
        assert_eq!(bytes, b"d1:eli203e21:agentnet: bad_contacte1:t2:ab1:y1:ee");
        assert_eq!(parse(&bytes), Some(Message::Other));
    }
}
