//! The frames of `agentnet-discovery/0`: the frozen vectors, and the length
//! prefix bounded before the body is read.

use pals_core::discovery::wire::{
    self, ErrorCode, GetInventory, Limits, Request, Response, request, response,
};
use pals_core::discovery::{
    ALPN_DISCOVERY, DISCOVERY_VERSION, MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_FRAME_BYTES,
    WireError, decode, encode_frame, frame_len, server_limits,
};
use serde_json::Value;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn vector() -> Value {
    let path = format!(
        "{}/tests/vectors/discovery-v0.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap()
}

/// The body of a frame whose bytes are `frame`: its prefix says how long it
/// is, and nothing follows it.
fn body_of(frame: &[u8], max: usize) -> &[u8] {
    let prefix: [u8; 4] = frame[..4].try_into().unwrap();
    let len = frame_len(prefix, max).unwrap();
    assert_eq!(frame.len(), 4 + len, "the prefix counts the whole body");
    &frame[4..]
}

#[test]
fn frozen_frame_vectors_match() {
    let vector = vector();
    assert_eq!(vector["alpn"], "agentnet-discovery/0");
    assert_eq!(ALPN_DISCOVERY, b"agentnet-discovery/0");
    assert_eq!(vector["version"], DISCOVERY_VERSION);
    assert_eq!(vector["max_request_frame_bytes"], MAX_REQUEST_FRAME_BYTES);
    assert_eq!(vector["max_response_frame_bytes"], MAX_RESPONSE_FRAME_BYTES);
    let frames = vector["frames"].as_array().unwrap();
    let frozen = |name: &str| -> Vec<u8> {
        let frame = frames
            .iter()
            .find(|frame| frame["name"] == name)
            .unwrap_or_else(|| panic!("frame {name}"));
        unhex(frame["hex"].as_str().unwrap())
    };

    // A request for the MCP section.
    let get = Request {
        version: 0,
        max_response_bytes: 0,
        grant_token: String::new(),
        op: Some(request::Op::GetInventory(GetInventory {
            sections: vec!["mcp".to_owned()],
            cursor: String::new(),
            page_entries: 0,
        })),
    };
    let bytes = frozen("get_inventory_request");
    assert_eq!(hex(&encode_frame(&get)), hex(&bytes));
    assert_eq!(
        decode::<Request>(body_of(&bytes, MAX_REQUEST_FRAME_BYTES)).unwrap(),
        get
    );

    // The refusal of another version, bare and as a server sends it.
    let refusal = |message: &str, limits: Option<Limits>| Response {
        version: 0,
        limits,
        contact_record: Vec::new(),
        announce_record: Vec::new(),
        result: Some(response::Result::Error(wire::Error {
            code: ErrorCode::UnsupportedVersion.into(),
            message: message.to_owned(),
            supported_versions: vec![DISCOVERY_VERSION],
            retry_after_ms: 0,
        })),
    };
    for (name, response) in [
        ("unsupported_version_error", refusal("", None)),
        (
            "unsupported_version_error_with_limits",
            refusal("unsupported version", Some(server_limits())),
        ),
    ] {
        let bytes = frozen(name);
        assert_eq!(hex(&encode_frame(&response)), hex(&bytes), "{name}");
        let decoded = decode::<Response>(body_of(&bytes, MAX_RESPONSE_FRAME_BYTES)).unwrap();
        assert_eq!(decoded, response, "{name}");
        let Some(response::Result::Error(error)) = decoded.result else {
            panic!("{name}: an error");
        };
        assert_eq!(error.code(), ErrorCode::UnsupportedVersion);
    }

    // The limits a server announces are the wire constants.
    assert_eq!(
        server_limits(),
        Limits {
            max_request_bytes: 20 * 1024,
            max_response_bytes: 80 * 1024,
            max_query_bytes: 8 * 1024,
            max_offer_bytes: 16 * 1024,
            max_page_bytes: 64 * 1024,
            max_page_entries: 100,
            max_query_total_bytes: 1024 * 1024,
            max_card_bytes: 32 * 1024,
        }
    );

    // Every code keeps its number.
    for (code, number) in [
        (ErrorCode::Unspecified, 0),
        (ErrorCode::UnsupportedVersion, 1),
        (ErrorCode::BadRequest, 2),
        (ErrorCode::TooLarge, 3),
        (ErrorCode::RateLimited, 4),
        (ErrorCode::NotFound, 5),
        (ErrorCode::InvalidCursor, 6),
        (ErrorCode::Unauthenticated, 7),
        (ErrorCode::UnsupportedOperation, 8),
        (ErrorCode::Internal, 9),
    ] {
        assert_eq!(i32::from(code), number, "{code:?}");
    }
}

#[test]
fn length_prefix_over_the_limit_is_too_large() {
    let prefix = |len: u32| len.to_be_bytes();
    let max = MAX_REQUEST_FRAME_BYTES;
    assert_eq!(frame_len(prefix(0), max), Ok(0));
    assert_eq!(frame_len(prefix(20 * 1024), max), Ok(20 * 1024));
    assert_eq!(
        frame_len(prefix(20 * 1024 + 1), max),
        Err(WireError::TooLarge)
    );
    // A gibibyte, or every bit set: refused from the four bytes alone.
    assert_eq!(frame_len(prefix(1 << 30), max), Err(WireError::TooLarge));
    assert_eq!(frame_len(prefix(u32::MAX), max), Err(WireError::TooLarge));
    // A response may be larger than a request.
    let response_max = MAX_RESPONSE_FRAME_BYTES;
    assert_eq!(frame_len(prefix(80 * 1024), response_max), Ok(80 * 1024));
    assert_eq!(
        frame_len(prefix(80 * 1024 + 1), response_max),
        Err(WireError::TooLarge)
    );

    // The prefix of an encoded frame is the length of its body, big-endian.
    let request = Request {
        version: 0,
        max_response_bytes: 0,
        grant_token: "t".repeat(300),
        op: None,
    };
    let frame = encode_frame(&request);
    let body_len = frame.len() - 4;
    assert_eq!(&frame[..4], &(body_len as u32).to_be_bytes());
    assert_eq!(
        decode::<Request>(&frame[4..]).unwrap().grant_token,
        "t".repeat(300)
    );

    // A body that is not a message does not decode.
    assert_eq!(decode::<Request>(&[0xff; 8]), Err(WireError::Malformed));
}
