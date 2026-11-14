//! What the gateway lets through of a peer's answer, and in what form.
//!
//! A stock client reads a peer's answer whole and builds its next URL from
//! the task ids in it: `<gateway>/peers/<peer>/rest/tasks/<id>`. A peer that
//! names a task `../../../<other>/rest/tasks/x` would steer that request, by
//! the client's own dot-segment normalization, to another peer, with the
//! grant the gateway holds for it. So nothing a peer sends reaches the
//! client as it was sent: the gateway parses it, checks it, and sends on
//! only its own encoding of what it checked. Parser differences between the
//! gateway and a client (duplicate keys, a byte order mark, `NaN`, UTF-16,
//! line breaks such as U+2028 or `\x1c` that some SSE readers split lines
//! on and others do not) then have nothing to hide in.
//!
//! **Unary or streaming, from the request.** What a client expects comes
//! from its own call, never from the peer's `Content-Type` ([`Call`]): on
//! the JSON-RPC binding, the `method` of a request object,
//! `SendStreamingMessage` or `SubscribeToTask` (the pinned SDK's
//! `methods::is_streaming`), and a batch (unless it asks for the extended
//! card, below) or a body that is not JSON is unary; on the HTTP+JSON
//! binding, the routes the pinned SDK's `rest_router` answers with a
//! stream: `POST /message:stream` and `/message/stream`, and `GET` or
//! `POST` on `/tasks/{id}` whose decoded `{id}` ends with `:subscribe` or
//! on `/tasks/{id}/subscribe` (that router serves no tenant-prefixed
//! paths). Only a streaming call whose `2xx`
//! answer is `text/event-stream` is read as events; anything else is read
//! whole, so a unary call answered with an event stream is refused as a
//! body that is not JSON.
//!
//! **The extended card** ([`Call::ExtendedCard`]): `GetExtendedAgentCard`
//! on JSON-RPC, `GET /extendedAgentCard` or `GET /agent-card/extended` on
//! HTTP+JSON. Its answer is read up to [`MAX_CARD_ANSWER_BYTES`]; a `2xx`
//! one must hold a card the peer signed, verified with `kid` equal to the
//! peer's agent id, the one Iroh's handshake proved (the card itself on
//! HTTP+JSON, the `result` of a JSON-RPC response), which is replaced by its
//! [`gateway_view`], built from what the signature covers
//! ([`extended_card_view`]). A JSON-RPC error goes on as any other. A card
//! that does not verify is refused, as a body that is not JSON is.
//!
//! **A whole body** is read up to the gateway's bound; a `2xx` one must be
//! empty or JSON (RFC 8259, UTF-8). JSON, of any status, is checked
//! ([`unsafe_id`]), its blob URLs become the gateway's, and it goes on
//! re-encoded ([`ascii_json`]), typed `application/json` (or
//! `application/a2a+json` when the peer said so). A non-`2xx` body that is
//! not JSON goes on as it came, bounded: a client takes no task id from an
//! error page.
//!
//! **An event stream** is split into events by [`EventFramer`], in time
//! linear in its bytes: an event ends at an empty line, lines end with
//! CRLF, LF or CR as SSE says, and one byte order mark at the start is
//! dropped. Each event goes on rebuilt from its data alone
//! ([`check_event`]): `data: <JSON>\n\n`, its `data` lines joined with LF
//! as SSE joins them, the JSON checked and re-encoded; an event with no
//! data becomes the fixed keepalive `:\n\n`, so that an idle stream stays
//! alive with no peer bytes in it. `event:`, `id:`, `retry:`, other fields
//! and comments are dropped: A2A carries everything in the data (the pinned
//! SDK server names no events; its `id:` counters serve no resumption, and
//! neither the TCK nor the Python SDK reads them). Data that is not JSON,
//! or names an unsafe id, ends the stream. An event the stream ends in the
//! middle of, before its empty line, is dropped, as SSE drops it.
//!
//! **Ids.** In the checked JSON, every string under `taskId` or `task_id`,
//! every element of `referenceTaskIds` or `reference_task_ids`, and the
//! `id` of every object must be a [safe](is_safe_task_id) task id: protobuf
//! JSON parsers, the Python SDK's among them, take both spellings of a
//! field, and a client takes any object in a task's place for a task, with
//! or without `contextId` and `status`. A non-string `id` is refused too.
//! Only the `id` of a JSON-RPC response envelope is the client's, and
//! passes: on the JSON-RPC binding only, at the top of a document (or of
//! each item of a batch), and only an object with `"jsonrpc": "2.0"` and a
//! `result` or an `error`. On HTTP+JSON there is no envelope, and an object
//! that wears a `jsonrpc` member is taken for what a client would take it:
//! a task, whose `id` it checks. The objects of A2A v1.0 with an `id` are the
//! task, the push notification configuration (not served: the card
//! declares no push notifications) and the agent skill (in an extended
//! card, whose skill ids PALS keeps safe); their ids are checked like a
//! task's. Skipped,
//! because no client takes an id from them: `metadata` (a free-form
//! object), `data` (a data part's free-form value, and a JSON-RPC error's)
//! and `details` (an HTTP+JSON error's `google.rpc.Status` details).
//!
//! Known limit: re-encoding goes through `serde_json`'s numbers, so a JSON
//! number beyond what `i64`, `u64` or `f64` hold exactly loses precision.
//! PALS workers answer with text and files; a data part with such numbers
//! would arrive rounded.

use std::io;

use bytes::{Bytes, BytesMut};
use http::Method;
use pals_core::card_jws::{CardHash, MAX_CARD_BYTES, card_hash, verify_card};
use pals_core::identity::AgentId;
use serde::Serialize;
use serde_json::Value;

use super::rewrite;
use crate::a2a::{gateway_view, is_safe_task_id};
use crate::discovery::signed_card_json;

/// The A2A binding a call speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Binding {
    JsonRpc,
    Rest,
}

/// What a client expects of a call: one JSON document, an event stream,
/// or the peer's extended card, one JSON document checked as a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Call {
    Unary,
    Streaming,
    ExtendedCard,
}

/// The JSON-RPC methods the pinned SDK answers with an event stream.
const STREAMING_METHODS: [&str; 2] = ["SendStreamingMessage", "SubscribeToTask"];
/// The JSON-RPC method of the extended card.
const EXTENDED_CARD_METHOD: &str = "GetExtendedAgentCard";
/// The most bytes of a peer's answer to an extended card call: the card, at
/// most [`MAX_CARD_BYTES`], and its JSON-RPC envelope, whose `id` is the
/// client's.
pub(crate) const MAX_CARD_ANSWER_BYTES: usize = 2 * MAX_CARD_BYTES;

impl Call {
    /// A call to the JSON-RPC binding whose body, when it is JSON, is
    /// `request`. A batch is unary, unless it asks for the extended card.
    pub(crate) fn jsonrpc(request: Option<&Value>) -> Self {
        fn method_of(request: &Value) -> Option<&str> {
            request
                .as_object()
                .and_then(|request| request.get("method"))
                .and_then(Value::as_str)
        }
        match request {
            Some(Value::Array(batch))
                if batch
                    .iter()
                    .any(|item| method_of(item) == Some(EXTENDED_CARD_METHOD)) =>
            {
                Self::ExtendedCard
            }
            Some(request) => match method_of(request) {
                Some(method) if STREAMING_METHODS.contains(&method) => Self::Streaming,
                Some(EXTENDED_CARD_METHOD) => Self::ExtendedCard,
                _ => Self::Unary,
            },
            None => Self::Unary,
        }
    }

    /// A call to the HTTP+JSON binding: `method` on `path`, the path after
    /// `/rest`, as sent (still percent-encoded).
    pub(crate) fn rest(method: &Method, path: &str) -> Self {
        let get_or_post = method == Method::GET || method == Method::POST;
        let segments: Vec<&str> = path.split('/').collect();
        if matches!(
            segments.as_slice(),
            ["", "extendedAgentCard"] | ["", "agent-card", "extended"]
        ) && method == Method::GET
        {
            return Self::ExtendedCard;
        }
        let streaming = match segments.as_slice() {
            ["", "message:stream"] | ["", "message", "stream"] => method == Method::POST,
            ["", "tasks", id] => {
                get_or_post
                    && !id.is_empty()
                    && percent_encoding::percent_decode_str(id)
                        .decode_utf8_lossy()
                        .ends_with(":subscribe")
            }
            ["", "tasks", id, "subscribe"] => get_or_post && !id.is_empty(),
            _ => false,
        };
        if streaming {
            Self::Streaming
        } else {
            Self::Unary
        }
    }
}

/// `answer`, a peer's successful answer to an extended card call on
/// `binding`, with its card verified as `peer`'s and replaced by its
/// [`gateway_view`] at `base_url`; the [hash](card_hash) of the card as
/// `peer` signed it ([`signed_card_json`]).
///
/// On HTTP+JSON the answer is the card. On JSON-RPC it is a response
/// envelope, with the card as its `result`, or with an `error`, which is
/// left as it is (`None`). Refused, saying why, when the card does not
/// verify as `peer`'s, or when the answer is not one of those.
pub(crate) fn extended_card_view(
    answer: &mut Value,
    binding: Binding,
    peer: &AgentId,
    base_url: &str,
) -> Result<Option<CardHash>, String> {
    let card = match binding {
        Binding::Rest => answer,
        Binding::JsonRpc => {
            let envelope = answer
                .as_object_mut()
                .filter(|envelope| is_response_envelope(envelope))
                .ok_or("an extended card answer that is not a JSON-RPC response")?;
            match envelope.get_mut("result") {
                Some(result) => result,
                None => return Ok(None),
            }
        }
    };
    let verified = verify_card(card, peer)
        .map_err(|err| format!("an extended card that is not the peer's: {err}"))?;
    let hash = card_hash(&signed_card_json(&verified));
    let view = gateway_view(&verified.card, base_url, peer);
    *card = serde_json::to_value(view).expect("an AgentCard always serializes");
    Ok(Some(hash))
}

/// The members whose values are free-form, and hold no id a client uses.
const FREE_FORM: [&str; 3] = ["metadata", "data", "details"];

/// The first id in `answer`, a JSON document a peer sent on `binding`,
/// that is not safe, as JSON: under `taskId` or `task_id`, in
/// `referenceTaskIds` or `reference_task_ids`, or the `id` of any object
/// but a JSON-RPC response envelope ([`is_response_envelope`]) at the top
/// (of the document, or of each item of a batch) on the JSON-RPC binding;
/// [`FREE_FORM`] members are not looked into.
pub(crate) fn unsafe_id(answer: &Value, binding: Binding) -> Option<String> {
    let jsonrpc = binding == Binding::JsonRpc;
    match answer {
        Value::Array(items) => items.iter().find_map(|item| unsafe_id_in(item, jsonrpc)),
        other => unsafe_id_in(other, jsonrpc),
    }
}

/// Whether `object` is a JSON-RPC 2.0 response: `"jsonrpc": "2.0"` and a
/// `result` or an `error`.
fn is_response_envelope(object: &serde_json::Map<String, Value>) -> bool {
    object.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && (object.contains_key("result") || object.contains_key("error"))
}

/// [`unsafe_id`] of `value`; its own `id` may be an envelope's when
/// `envelope_allowed`.
fn unsafe_id_in(value: &Value, envelope_allowed: bool) -> Option<String> {
    match value {
        Value::Object(object) => {
            let envelope = envelope_allowed && is_response_envelope(object);
            for (key, member) in object {
                let refused = match key.as_str() {
                    free if FREE_FORM.contains(&free) => continue,
                    "id" if envelope => None,
                    "id" => {
                        (!member.as_str().is_some_and(is_safe_task_id)).then(|| member.to_string())
                    }
                    "taskId" | "task_id" => member
                        .as_str()
                        .filter(|id| !is_safe_task_id(id))
                        .map(str::to_owned),
                    "referenceTaskIds" | "reference_task_ids" => match member {
                        Value::Array(ids) => ids
                            .iter()
                            .find(|id| !id.as_str().is_some_and(is_safe_task_id))
                            .map(Value::to_string),
                        Value::Null => None,
                        other => Some(other.to_string()),
                    },
                    _ => None,
                };
                if refused.is_some() {
                    return refused;
                }
                if let Some(found) = unsafe_id_in(member, false) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|item| unsafe_id_in(item, false)),
        _ => None,
    }
}

/// `value` as compact JSON in ASCII only: every other character escaped as
/// `\uXXXX`, those beyond U+FFFF as a surrogate pair.
pub(crate) fn ascii_json(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, AsciiOnly);
    value
        .serialize(&mut serializer)
        .expect("a JSON value serializes into memory");
    out
}

/// `serde_json`'s compact format, but for the characters beyond ASCII in
/// strings, which it escapes.
struct AsciiOnly;

impl serde_json::ser::Formatter for AsciiOnly {
    fn write_string_fragment<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut plain = 0;
        for (at, character) in fragment.char_indices() {
            if character.is_ascii() {
                continue;
            }
            writer.write_all(&fragment.as_bytes()[plain..at])?;
            let mut units = [0u16; 2];
            for unit in character.encode_utf16(&mut units) {
                write!(writer, "\\u{unit:04x}")?;
            }
            plain = at + character.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[plain..])
    }
}

/// The keepalive an event without data becomes.
pub(crate) const KEEPALIVE: &[u8] = b":\n\n";

/// Splits an event stream into events as it arrives, looking at each byte
/// a bounded number of times however it is cut into chunks, and taking each
/// event off the front of what it holds without moving the rest.
pub(crate) struct EventFramer {
    /// The event being received, and whatever came after it.
    pending: BytesMut,
    /// How much of `pending` has been looked at.
    scanned: usize,
    /// Where in `pending` the line being received starts.
    line_start: usize,
    /// Whether the stream's first bytes, and so a byte order mark, may still
    /// come.
    at_start: bool,
    /// Bytes looked at, in all.
    #[cfg(test)]
    looked: usize,
}

/// The byte order mark SSE ignores before the first event.
const BOM: &[u8] = "\u{feff}".as_bytes();

impl EventFramer {
    pub(crate) fn new() -> Self {
        Self {
            pending: BytesMut::new(),
            scanned: 0,
            line_start: 0,
            at_start: true,
            #[cfg(test)]
            looked: 0,
        }
    }

    /// Takes `chunk`, the next bytes of the stream.
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
    }

    /// The bytes held: the event being received, from its first line.
    pub(crate) fn held(&self) -> usize {
        self.pending.len()
    }

    /// The next complete event, its empty line included, once it is all
    /// there. With `ended`, the stream has ended: a CR at the end is a line
    /// end.
    pub(crate) fn next_event(&mut self, ended: bool) -> Option<BytesMut> {
        if self.at_start {
            if self.pending.len() < BOM.len() && BOM.starts_with(&self.pending) && !ended {
                return None;
            }
            if self.pending.starts_with(BOM) {
                let _ = self.pending.split_to(BOM.len());
            }
            self.at_start = false;
        }
        loop {
            let rest = &self.pending[self.scanned..];
            let Some(offset) = rest.iter().position(|&b| b == b'\r' || b == b'\n') else {
                self.count(rest.len());
                self.scanned = self.pending.len();
                return None;
            };
            self.count(offset + 1);
            let at = self.scanned + offset;
            let after = match (self.pending[at], self.pending.get(at + 1)) {
                (b'\r', Some(b'\n')) => at + 2,
                (b'\r', None) if !ended => {
                    // A LF may follow: the CR is looked at again then.
                    self.scanned = at;
                    return None;
                }
                _ => at + 1,
            };
            let empty_line = at == self.line_start;
            self.scanned = after;
            self.line_start = after;
            if empty_line {
                let event = self.pending.split_to(after);
                self.scanned = 0;
                self.line_start = 0;
                return Some(event);
            }
        }
    }

    /// What is left once the stream has ended and every complete event was
    /// taken: an event cut short, if any.
    pub(crate) fn rest(&mut self) -> Option<BytesMut> {
        if self.pending.is_empty() {
            return None;
        }
        self.scanned = 0;
        self.line_start = 0;
        Some(self.pending.split())
    }

    #[cfg(test)]
    fn count(&mut self, bytes: usize) {
        self.looked += bytes;
    }

    #[cfg(not(test))]
    fn count(&mut self, _bytes: usize) {}
}

/// `event`, one event of a peer's stream on `binding`, as it goes to the
/// client: `data: <JSON>\n\n` with its data checked ([`unsafe_id`]), its
/// blob URLs the gateway's and re-encoded ([`ascii_json`]), or
/// [`KEEPALIVE`] when it has no data. Refused when it is not UTF-8, its
/// data is not JSON, or the data names an unsafe id.
pub(crate) fn check_event(event: &[u8], base_url: &str, binding: Binding) -> Result<Bytes, String> {
    let text = std::str::from_utf8(event).map_err(|_| "an event that is not UTF-8".to_owned())?;
    let data: Vec<&str> = sse_lines(text).into_iter().filter_map(sse_data).collect();
    if data.is_empty() {
        return Ok(Bytes::from_static(KEEPALIVE));
    }
    let mut value = serde_json::from_str::<Value>(&data.join("\n"))
        .map_err(|_| "an event whose data is not JSON".to_owned())?;
    if let Some(id) = unsafe_id(&value, binding) {
        return Err(format!("an event with an unsafe id {id}"));
    }
    rewrite::rewrite_to_gateway(&mut value, base_url);
    let mut out = b"data: ".to_vec();
    out.extend_from_slice(&ascii_json(&value));
    out.extend_from_slice(b"\n\n");
    Ok(Bytes::from(out))
}

/// The lines of `text`, without their endings (CRLF, LF or CR).
fn sse_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let Some(at) = rest.find(['\r', '\n']) else {
            lines.push(rest);
            break;
        };
        let ending = if rest[at..].starts_with("\r\n") { 2 } else { 1 };
        lines.push(&rest[..at]);
        rest = &rest[at + ending..];
    }
    lines
}

/// The value of `line` when it is a `data` field, as SSE reads it: after
/// `data:`, less one space; `""` for a bare `data`.
fn sse_data(line: &str) -> Option<&str> {
    match line.split_once(':') {
        Some(("data", value)) => Some(value.strip_prefix(' ').unwrap_or(value)),
        None if line == "data" => Some(""),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn calls_are_streaming_as_the_sdk_routes_answer_them() {
        let streaming = [
            (Method::POST, "/message:stream"),
            (Method::POST, "/message/stream"),
            (Method::GET, "/tasks/t-1:subscribe"),
            (Method::POST, "/tasks/t-1:subscribe"),
            (Method::GET, "/tasks/t-1%3Asubscribe"),
            (Method::GET, "/tasks/t-1/subscribe"),
            (Method::POST, "/tasks/t-1/subscribe"),
        ];
        for (method, path) in streaming {
            assert_eq!(
                Call::rest(&method, path),
                Call::Streaming,
                "{method} {path}"
            );
        }
        let unary = [
            (Method::POST, "/message:send"),
            (Method::POST, "/message/send"),
            (Method::GET, "/message:stream"),
            (Method::POST, "/message:stream/"),
            (Method::GET, "/tasks/t-1"),
            (Method::POST, "/tasks/t-1:cancel"),
            (Method::DELETE, "/tasks/t-1:subscribe"),
            (Method::GET, "/tasks"),
            (Method::GET, "/tasks/t-1/pushNotificationConfigs"),
            (Method::GET, "/tenant/message:stream"),
            (Method::GET, ""),
        ];
        for (method, path) in unary {
            assert_eq!(Call::rest(&method, path), Call::Unary, "{method} {path}");
        }
        for method in ["SendStreamingMessage", "SubscribeToTask"] {
            let request = json!({"jsonrpc": "2.0", "id": 1, "method": method});
            assert_eq!(Call::jsonrpc(Some(&request)), Call::Streaming, "{method}");
        }
        for request in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "SendMessage"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "GetTask"}),
            json!([{"jsonrpc": "2.0", "id": 1, "method": "SendStreamingMessage"}]),
            json!({"method": 1}),
            json!("SendStreamingMessage"),
        ] {
            assert_eq!(Call::jsonrpc(Some(&request)), Call::Unary, "{request}");
        }
        assert_eq!(Call::jsonrpc(None), Call::Unary);
    }

    #[test]
    fn extended_card_calls_are_recognized_on_both_bindings() {
        for path in ["/extendedAgentCard", "/agent-card/extended"] {
            assert_eq!(Call::rest(&Method::GET, path), Call::ExtendedCard, "{path}");
            for method in [Method::HEAD, Method::POST] {
                assert_eq!(Call::rest(&method, path), Call::Unary, "{method} {path}");
            }
        }
        for path in [
            "/extendedAgentCard/",
            "/tasks/extendedAgentCard",
            "/agent-card",
        ] {
            assert_eq!(Call::rest(&Method::GET, path), Call::Unary, "{path}");
        }
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "GetExtendedAgentCard"});
        assert_eq!(Call::jsonrpc(Some(&call)), Call::ExtendedCard);
        let batch = json!([{"method": "GetTask"}, call]);
        assert_eq!(Call::jsonrpc(Some(&batch)), Call::ExtendedCard);
        let other = json!({"jsonrpc": "2.0", "id": 1, "method": "getExtendedAgentCard"});
        assert_eq!(Call::jsonrpc(Some(&other)), Call::Unary);
    }

    #[test]
    fn only_a_card_the_peer_signed_goes_on_as_its_view() {
        use std::sync::Arc;

        use pals_core::card_jws::CardSigner;
        use pals_core::identity::AgentKey;
        use pals_core::inventory::Inventory;

        let key = AgentKey::generate();
        let peer = key.id();
        let cfg: crate::config::Config = toml::from_str("[agent]\nname = \"bob\"\n").unwrap();
        let signer = CardSigner::new(Arc::new(AgentKey::from_bytes(&key.to_bytes())));
        let cards = crate::a2a::CardSet::build(&cfg, &signer, &Inventory::default()).unwrap();
        let signed = cards.public();
        let base = "http://127.0.0.1:7420";
        let view = serde_json::to_value(gateway_view(&signed.card, base, &peer)).unwrap();

        let mut rest = signed.json.clone();
        let hash = extended_card_view(&mut rest, Binding::Rest, &peer, base).unwrap();
        assert_eq!(hash, Some(signed.hash.clone()));
        assert_eq!(rest, view);
        let mut rpc = json!({"jsonrpc": "2.0", "id": "x", "result": signed.json});
        let hash = extended_card_view(&mut rpc, Binding::JsonRpc, &peer, base).unwrap();
        assert_eq!(hash, Some(signed.hash.clone()));
        assert_eq!(rpc, json!({"jsonrpc": "2.0", "id": "x", "result": view}));

        // A JSON-RPC error goes on as it is.
        let error = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32004}});
        let mut answer = error.clone();
        assert_eq!(
            extended_card_view(&mut answer, Binding::JsonRpc, &peer, base),
            Ok(None)
        );
        assert_eq!(answer, error);

        // Refused: another agent's card, a tampered one, no card at all.
        let mut tampered = signed.json.clone();
        tampered["name"] = json!("mallory");
        for (mut answer, binding, signer) in [
            (
                signed.json.clone(),
                Binding::Rest,
                AgentKey::generate().id(),
            ),
            (tampered.clone(), Binding::Rest, peer),
            (
                json!({"jsonrpc": "2.0", "id": 1, "result": tampered}),
                Binding::JsonRpc,
                peer,
            ),
            (json!({"result": signed.json}), Binding::JsonRpc, peer),
            (
                json!([{"jsonrpc": "2.0", "id": 1, "result": signed.json}]),
                Binding::JsonRpc,
                peer,
            ),
            (json!({}), Binding::Rest, peer),
        ] {
            assert!(
                extended_card_view(&mut answer, binding, &signer, base).is_err(),
                "{answer}"
            );
        }
    }

    #[test]
    fn every_spelling_and_every_object_is_checked() {
        let bad = "../x";
        let refused = [
            json!({"task": {"id": bad}}),
            json!({"id": bad}),
            json!({"id": bad, "context_id": "c"}),
            json!({"statusUpdate": {"task_id": bad}}),
            json!({"artifactUpdate": {"taskId": bad}}),
            json!({"message": {"referenceTaskIds": ["t-1", bad]}}),
            json!({"message": {"reference_task_ids": [bad]}}),
            json!({"message": {"referenceTaskIds": [1]}}),
            json!({"message": {"referenceTaskIds": "t-1"}}),
            json!({"tasks": [{"id": "t-1"}, {"id": bad}]}),
            json!({"task": {"id": 7}}),
            json!({"task": {"id": null}}),
            json!({"jsonrpc": "2.0", "id": 1, "result": {"task": {"id": bad}}}),
            json!({"jsonrpc": "2.0", "id": 1, "result": {"id": "../y"}}),
            json!([{"jsonrpc": "2.0", "id": 1, "result": {"task_id": bad}}]),
            // An envelope only at the top.
            json!({"result": {"jsonrpc": "2.0", "id": bad, "result": {}}}),
            // Not a response envelope: a task wearing a `jsonrpc` member.
            json!({"jsonrpc": "2.0", "id": bad, "contextId": "c", "status": {}}),
            json!({"jsonrpc": "1.0", "id": bad, "result": {}}),
        ];
        for binding in [Binding::JsonRpc, Binding::Rest] {
            for answer in &refused {
                assert!(unsafe_id(answer, binding).is_some(), "{binding:?} {answer}");
            }
        }
        let passed = [
            json!({"task": {"id": "t-1", "contextId": "c", "status": {}}}),
            json!({"task": {"id": "t-1", "metadata": {"id": bad, "taskId": bad}}}),
            json!({"parts": [{"data": {"id": bad, "task_id": bad}}]}),
            json!({"error": {"details": [{"id": bad}]}}),
            json!({"message": {"taskId": null, "referenceTaskIds": null}}),
            json!({"message": {"messageId": "../m", "contextId": "../c"}}),
        ];
        for binding in [Binding::JsonRpc, Binding::Rest] {
            for answer in &passed {
                assert_eq!(unsafe_id(answer, binding), None, "{binding:?} {answer}");
            }
        }
        // The client's own id, in a response envelope, on JSON-RPC only.
        let envelopes = [
            json!({"jsonrpc": "2.0", "id": bad, "result": {"task": {"id": "t-1"}}}),
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": 1, "data": {"id": bad}}}),
            json!([{"jsonrpc": "2.0", "id": 3, "result": {}}]),
        ];
        for envelope in &envelopes {
            assert_eq!(unsafe_id(envelope, Binding::JsonRpc), None, "{envelope}");
            assert!(unsafe_id(envelope, Binding::Rest).is_some(), "{envelope}");
        }
    }

    #[test]
    fn json_is_reencoded_in_ascii_only() {
        let value = json!({"ñ": "ñandú 😀 \u{2028}\u{85}\u{b}\u{1c}", "n": [1, -2, 0.5]});
        let encoded = ascii_json(&value);
        assert!(encoded.is_ascii(), "{}", String::from_utf8_lossy(&encoded));
        let text = String::from_utf8(encoded).unwrap();
        assert!(text.contains(r"\ud83d\ude00"), "{text}");
        assert!(text.contains(r"\u2028"), "{text}");
        assert!(text.contains(r"\u000b"), "{text}");
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), value);
    }

    /// Feeds `input` to a framer in chunks of `chunk` bytes; the events it
    /// gave, and the bytes it looked at.
    fn frame(input: &[u8], chunk: usize) -> (Vec<BytesMut>, usize) {
        let mut framer = EventFramer::new();
        let mut events = Vec::new();
        for piece in input.chunks(chunk) {
            framer.push(piece);
            while let Some(event) = framer.next_event(false) {
                events.push(event);
            }
        }
        while let Some(event) = framer.next_event(true) {
            events.push(event);
        }
        events.extend(framer.rest());
        (events, framer.looked)
    }

    #[test]
    fn framing_looks_at_each_byte_a_bounded_number_of_times() {
        // A line that never ends, fed in 1 KiB chunks.
        let line = vec![b'x'; 16 * 1024 * 1024];
        let (events, looked) = frame(&line, 1024);
        assert_eq!(events.len(), 1);
        assert!(looked <= 2 * line.len(), "{looked} for {}", line.len());

        // Many small events, in one chunk and in chunks cutting them.
        let keepalives = ":\n\n".repeat(400 * 1024 / 3).into_bytes();
        for chunk in [keepalives.len(), 1024, 7] {
            let (events, looked) = frame(&keepalives, chunk);
            assert_eq!(events.len(), keepalives.len() / 3, "{chunk}");
            assert!(looked <= 2 * keepalives.len(), "{looked} for {chunk}");
        }

        // CR at every chunk's end.
        let crs = "data: x\r\r".repeat(10_000).into_bytes();
        let (events, looked) = frame(&crs, 9);
        assert_eq!(events.len(), 10_000);
        assert!(looked <= 2 * crs.len(), "{looked}");
    }

    #[test]
    fn events_end_at_an_empty_line_whatever_the_line_ends() {
        for ending in ["\n", "\r\n", "\r"] {
            let input = format!("data: 1{ending}{ending}: c{ending}data: 2{ending}{ending}tail");
            for chunk in [1, 2, 3, input.len()] {
                let (events, _) = frame(input.as_bytes(), chunk);
                let events: Vec<String> = events
                    .iter()
                    .map(|e| String::from_utf8(e.to_vec()).unwrap())
                    .collect();
                assert_eq!(
                    events,
                    [
                        format!("data: 1{ending}{ending}"),
                        format!(": c{ending}data: 2{ending}{ending}"),
                        "tail".to_owned()
                    ],
                    "{ending:?} in chunks of {chunk}"
                );
            }
        }
        // A byte order mark before the first event is dropped.
        let (events, _) = frame("\u{feff}data: 1\n\n".as_bytes(), 1);
        assert_eq!(events[0].as_ref(), b"data: 1\n\n");
    }

    #[test]
    fn events_go_on_rebuilt_from_their_checked_data() {
        let base = "http://127.0.0.1:7420";
        let check = |event: &str| check_event(event.as_bytes(), base, Binding::Rest);
        // Fields, comments and ids go; data, joined, stays.
        let rebuilt =
            check("event: x\r\nid: 7\r\n: c\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n").unwrap();
        assert_eq!(rebuilt.as_ref(), b"data: {\"a\":1}\n\n");
        // No data: the fixed keepalive, whatever the event held.
        for no_data in [
            ":\n\n",
            ": hi\u{2028}data: {\"task\":{\"id\":\"../x\"}}\n\n",
            "event: x\u{1c}data: {}\n\n",
            "id: 7\u{85}data: {}\n\n",
            "foo: bar\u{b}data: {}\n\n",
            "\n",
        ] {
            assert_eq!(check(no_data).unwrap().as_ref(), KEEPALIVE, "{no_data:?}");
        }
        // Refused: not JSON, lenient JSON, an unsafe id, not UTF-8.
        for refused in [
            "data: not json\n\n",
            "data: {\"task\": {\"id\": \"t\", \"n\": NaN}}\n\n",
            "data: {\"statusUpdate\": {\"task_id\": \"../x\"}}\n\n",
            "data: {\"task\":\ndata: {\"id\": \"../x\"}}\n\n",
        ] {
            assert!(check(refused).is_err(), "{refused:?}");
        }
        assert!(check_event(b"data: \xff\n\n", base, Binding::Rest).is_err());
    }
}
