//! The A2A v1.0 server from the official SDK, unmodified, served over an Iroh
//! stream: one HTTP/1.1 exchange per bidirectional stream.

mod common;

use axum::body::{Body, Bytes};
use axum::http::{Request, header};
use http_body_util::BodyExt;
use pals::net::{PEER_HEADER, TunnelLimits};
use serde_json::{Value, json};

fn send_message(method: &str, message_id: &str, text: &str) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":{
        "message":{"messageId":message_id,"role":"ROLE_USER","parts":[{"text":text}]}}})
}

#[tokio::test]
async fn card_is_served_over_iroh() {
    let (server, client) = common::spawn_echo_pair().await; // registers TunnelServer on ALPN_A2A_HTTP
    let res = client.get(&server, "/.well-known/agent-card.json").await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json["name"], "echo");
}

#[tokio::test]
async fn send_message_roundtrip_jsonrpc() {
    let (server, client) = common::spawn_echo_pair().await;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{
        "message":{"messageId":"m-1","role":"ROLE_USER","parts":[{"text":"hola"}]}}});
    let res = client
        .post_json(&server, "/jsonrpc", &[("a2a-version", "1.0")], &body)
        .await;
    assert_eq!(res.status, 200);
    let task = &res.json["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(task["artifacts"][0]["parts"][0]["text"], "echo: hola");
}

#[tokio::test]
async fn streaming_delivers_events_before_the_stream_ends() {
    let (server, client) = common::spawn_echo_pair().await;
    let body = send_message("SendStreamingMessage", "m-1", "hola");
    let request = Request::post("/jsonrpc")
        .header(header::CONTENT_TYPE, "application/json")
        .header("a2a-version", "1.0")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = client.send(&server, request).await;
    assert_eq!(res.status(), 200);
    let content_type = res.headers()[header::CONTENT_TYPE].to_str().unwrap();
    assert!(
        content_type.starts_with("text/event-stream"),
        "{content_type}"
    );

    // Read the body frame by frame, as it leaves the tunnel.
    let mut body = res.into_body();
    let mut pending = String::new();
    let mut events: Vec<Value> = Vec::new();
    let mut frames = 0;
    let mut first_event_at_frame = None;
    while let Some(frame) = body.frame().await {
        let Ok(data) = frame.expect("SSE body frame").into_data() else {
            continue;
        };
        frames += 1;
        pending.push_str(std::str::from_utf8(&data).expect("SSE is UTF-8"));
        while let Some(end) = pending.find("\n\n") {
            let raw_event: String = pending.drain(..end + 2).collect();
            events.extend(sse_data(&raw_event));
        }
        if first_event_at_frame.is_none() && !events.is_empty() {
            first_event_at_frame = Some(frames);
        }
    }

    let first_event_at_frame = first_event_at_frame.expect("the first SSE event arrives");
    assert!(
        first_event_at_frame < frames,
        "the first event must arrive before the stream ends, not with its last frame \
         (first event at frame {first_event_at_frame} of {frames})"
    );
    assert_eq!(events[0]["id"], 1);
    assert!(events[0]["result"].is_object(), "{}", events[0]);
    let last = &events.last().unwrap()["result"];
    let final_state = last
        .pointer("/statusUpdate/status/state")
        .or_else(|| last.pointer("/task/status/state"));
    assert_eq!(final_state, Some(&json!("TASK_STATE_COMPLETED")), "{last}");
}

#[tokio::test]
async fn client_cannot_spoof_peer_header() {
    // The test app returns the x-agentnet-peer values it sees; the client sends false ones.
    let (server, client) = common::spawn_pair(common::peer_header_app()).await;
    let fake = "0".repeat(64);
    let res = client
        .post_json(
            &server,
            "/peer",
            &[(PEER_HEADER, fake.as_str()), (PEER_HEADER, "someone-else")],
            &json!({}),
        )
        .await;
    assert_eq!(res.status, 200);
    let seen = res.json["peer"].as_array().expect("list of peer values");
    assert_eq!(seen.len(), 1, "exactly one value survives: {seen:?}");
    let seen_peer = seen[0].as_str().unwrap();

    let client_endpoint_id_hex: String = client
        .endpoint
        .id()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(client_endpoint_id_hex.len(), 64);
    assert_eq!(seen_peer, client_endpoint_id_hex);
}

#[tokio::test]
async fn body_over_limit_is_rejected_with_413() {
    let (server, client) = common::spawn_echo_pair().await;
    let limit = TunnelLimits::default().max_body_bytes;
    assert_eq!(limit, 1024 * 1024);

    // A JSON string of n characters serializes to n + 2 bytes.
    let over = Value::String("x".repeat(limit - 1));
    assert_eq!(serde_json::to_vec(&over).unwrap().len(), limit + 1);
    let res = client
        .post_json(&server, "/jsonrpc", &[("a2a-version", "1.0")], &over)
        .await;
    assert_eq!(res.status, 413);

    // Without a Content-Length (chunked body) the limit holds as well.
    let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::from(vec![b' '; limit])),
        Ok(Bytes::from_static(b" ")),
    ];
    let request = Request::post("/jsonrpc")
        .header(header::CONTENT_TYPE, "application/json")
        .header("a2a-version", "1.0")
        .body(Body::from_stream(futures::stream::iter(chunks)))
        .unwrap();
    let res = client.send(&server, request).await;
    assert_eq!(res.status(), 413);

    // Exactly at the limit passes the tunnel; the JSON-RPC binding then rejects
    // the non-object body inside a 200 response.
    let at_limit = Value::String("x".repeat(limit - 2));
    assert_eq!(serde_json::to_vec(&at_limit).unwrap().len(), limit);
    let res = client
        .post_json(&server, "/jsonrpc", &[("a2a-version", "1.0")], &at_limit)
        .await;
    assert_eq!(res.status, 200);
    assert!(res.json["error"]["code"].is_i64(), "{}", res.json);
}

#[tokio::test]
async fn version_other_than_1_0_is_rejected() {
    // a2a-version: 9.9 -> JSON-RPC error with code -32009
    let (server, client) = common::spawn_echo_pair().await;
    let body = send_message("SendMessage", "m-1", "hola");
    let res = client
        .post_json(&server, "/jsonrpc", &[("a2a-version", "9.9")], &body)
        .await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json["error"]["code"], -32009, "{}", res.json);
}

/// The JSON in an SSE event's `data:` lines, or `None` for an event without
/// data (a keep-alive comment).
fn sse_data(raw_event: &str) -> Option<Value> {
    let data: Vec<&str> = raw_event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .collect();
    if data.is_empty() {
        return None;
    }
    Some(serde_json::from_str(&data.join("\n")).expect("SSE data is JSON"))
}
