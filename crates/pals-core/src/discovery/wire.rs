//! The messages of `agentnet-discovery/0` and their frames.
//!
//! The messages are those of `docs/protocols/agentnet-discovery-0.proto`,
//! package `agentnet.discovery.v0`, written by hand with `prost`'s derive:
//! their field numbers and enumeration values are frozen there, and the
//! vector `tests/vectors/discovery-v0.json` freezes their encoding.
//!
//! A frame is the length of a message's protobuf encoding, as a `u32`
//! big-endian, and then that encoding ([`encode_frame`]). A reader takes the
//! four bytes of the length first and refuses a frame longer than its limit
//! before reading any of the body ([`frame_len`]).

use prost::Message;

/// A request: one per stream, which the requester closes after it.
#[derive(Clone, PartialEq, Message)]
pub struct Request {
    /// The protocol version the requester speaks: [`super::DISCOVERY_VERSION`].
    #[prost(uint32, tag = "1")]
    pub version: u32,
    /// The largest response body the requester reads; 0 for the server's
    /// own limit, which also caps it.
    #[prost(uint32, tag = "2")]
    pub max_response_bytes: u32,
    /// An `agentnet.grant/1` token, or empty: at most
    /// [`crate::grant::MAX_TOKEN_BYTES`].
    #[prost(string, tag = "3")]
    pub grant_token: String,
    #[prost(oneof = "request::Op", tags = "10, 11, 12")]
    pub op: Option<request::Op>,
}

/// The operations of a [`Request`].
pub mod request {
    /// What a request asks for.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Op {
        #[prost(message, tag = "10")]
        GetInventory(super::GetInventory),
        #[prost(message, tag = "11")]
        GetBenchmarkReport(super::GetBenchmarkReport),
        #[prost(message, tag = "12")]
        ServiceQuery(super::ServiceQuery),
    }
}

/// A page of the requester's view of the inventory.
#[derive(Clone, PartialEq, Message)]
pub struct GetInventory {
    /// The sections, by their names (`services`, `mcp`...); all when empty.
    #[prost(string, repeated, tag = "1")]
    pub sections: Vec<String>,
    /// The previous page's `next_cursor`; empty for the first page.
    #[prost(string, tag = "2")]
    pub cursor: String,
    /// The most entries the page holds; 0 for the most a page holds.
    #[prost(uint32, tag = "3")]
    pub page_entries: u32,
}

/// A benchmark report the agent holds.
#[derive(Clone, PartialEq, Message)]
pub struct GetBenchmarkReport {
    #[prost(string, tag = "1")]
    pub benchmark_id: String,
}

/// A query for a service.
#[derive(Clone, PartialEq, Message)]
pub struct ServiceQuery {
    /// The JCS of an `agentnet.service-query/1`.
    #[prost(bytes = "vec", tag = "1")]
    pub query: Vec<u8>,
}

/// A response: one per stream, which the server closes after it.
#[derive(Clone, PartialEq, Message)]
pub struct Response {
    /// The protocol version of the response: [`super::DISCOVERY_VERSION`].
    #[prost(uint32, tag = "1")]
    pub version: u32,
    /// The server's limits.
    #[prost(message, optional, tag = "2")]
    pub limits: Option<Limits>,
    /// The server's signed contact record, as its JSON.
    #[prost(bytes = "vec", tag = "3")]
    pub contact_record: Vec<u8>,
    /// The server's signed announce record, as its JSON.
    #[prost(bytes = "vec", tag = "4")]
    pub announce_record: Vec<u8>,
    #[prost(oneof = "response::Result", tags = "10, 11, 12, 15")]
    pub result: Option<response::Result>,
}

/// The results of a [`Response`].
pub mod response {
    /// What a response carries: what was asked for, or why not.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Result {
        /// A signed `agentnet.inventory-view/1` page, as its JSON.
        #[prost(bytes = "vec", tag = "10")]
        InventoryPage(Vec<u8>),
        /// A signed `agentnet.benchmark-report/1`, as its JSON.
        #[prost(bytes = "vec", tag = "11")]
        BenchmarkReport(Vec<u8>),
        /// A signed `agentnet.service-offer/1`, as its JSON.
        #[prost(bytes = "vec", tag = "12")]
        ServiceOffer(Vec<u8>),
        #[prost(message, tag = "15")]
        Error(super::Error),
    }
}

/// What a server takes and gives, in bytes or entries.
#[derive(Clone, Copy, PartialEq, Eq, Message)]
pub struct Limits {
    #[prost(uint32, tag = "1")]
    pub max_request_bytes: u32,
    #[prost(uint32, tag = "2")]
    pub max_response_bytes: u32,
    #[prost(uint32, tag = "3")]
    pub max_query_bytes: u32,
    #[prost(uint32, tag = "4")]
    pub max_offer_bytes: u32,
    #[prost(uint32, tag = "5")]
    pub max_page_bytes: u32,
    #[prost(uint32, tag = "6")]
    pub max_page_entries: u32,
    #[prost(uint32, tag = "7")]
    pub max_query_total_bytes: u32,
    #[prost(uint32, tag = "8")]
    pub max_card_bytes: u32,
}

/// Why a request was not answered with what it asked for. `message` is at
/// most [`super::MAX_ERROR_MESSAGE_BYTES`] and never names anything the
/// requester may not see.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct Error {
    #[prost(enumeration = "ErrorCode", tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub message: String,
    /// With [`ErrorCode::UnsupportedVersion`]: the versions the server
    /// speaks.
    #[prost(uint32, repeated, tag = "3")]
    pub supported_versions: Vec<u32>,
    /// With [`ErrorCode::RateLimited`]: when to try again, in milliseconds.
    #[prost(uint32, tag = "4")]
    pub retry_after_ms: u32,
}

/// The kinds of [`Error`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum ErrorCode {
    Unspecified = 0,
    UnsupportedVersion = 1,
    BadRequest = 2,
    TooLarge = 3,
    RateLimited = 4,
    NotFound = 5,
    InvalidCursor = 6,
    Unauthenticated = 7,
    UnsupportedOperation = 8,
    Internal = 9,
}

impl Error {
    /// An error of `code` saying `message`, cut to
    /// [`super::MAX_ERROR_MESSAGE_BYTES`] at a character boundary.
    pub fn new(code: ErrorCode, message: &str) -> Self {
        let mut end = message.len().min(super::MAX_ERROR_MESSAGE_BYTES);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            code: code.into(),
            message: message[..end].to_owned(),
            supported_versions: Vec::new(),
            retry_after_ms: 0,
        }
    }
}

/// Why a frame was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The length prefix says more than the reader's limit.
    #[error("frame too large")]
    TooLarge,
    /// The body is not the message expected.
    #[error("malformed frame")]
    Malformed,
}

/// `msg` as a frame: the length of its encoding, as a `u32` big-endian, and
/// the encoding.
///
/// # Panics
///
/// If the encoding is longer than `u32::MAX` bytes, which no message of the
/// protocol can be.
pub fn encode_frame(msg: &impl Message) -> Vec<u8> {
    let len = msg.encoded_len();
    let prefix = u32::try_from(len).expect("a message of at most 4 GiB");
    let mut frame = Vec::with_capacity(4 + len);
    frame.extend_from_slice(&prefix.to_be_bytes());
    msg.encode_raw(&mut frame);
    frame
}

/// The body length `prefix` announces, when it is at most `max`;
/// [`WireError::TooLarge`] otherwise, before any of the body is read.
pub fn frame_len(prefix: [u8; 4], max: usize) -> Result<usize, WireError> {
    usize::try_from(u32::from_be_bytes(prefix))
        .ok()
        .filter(|&len| len <= max)
        .ok_or(WireError::TooLarge)
}

/// The message whose encoding is `body`, the body of a frame.
pub fn decode<M: Message + Default>(body: &[u8]) -> Result<M, WireError> {
    M::decode(body).map_err(|_| WireError::Malformed)
}
