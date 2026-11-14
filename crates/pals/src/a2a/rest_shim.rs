//! [`rest_shim`]: what the SDK's HTTP+JSON binding lacks for A2A
//! conformance, made up for in front of its router.

use std::collections::HashMap;

use a2a::errordetails::{ERROR_INFO_TYPE, PROTOCOL_DOMAIN};
use a2a::{A2AError, error_code, error_reason};
use axum::Json;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue, StatusCode};
use serde_json::json;

/// The service parameter that names the client's protocol version.
const VERSION_HEADER: &str = "a2a-version";
/// What an absent [`VERSION_HEADER`] means (A2A §3.6.1, §3.6.2).
const ABSENT_VERSION: &str = "0.3";
/// The major protocol version the SDK serves.
const SUPPORTED_MAJOR_VERSION: u32 = 1;
/// The content type the SDK gives HTTP+JSON errors.
const PROBLEM_JSON: &str = "application/problem+json";
/// The content type HTTP+JSON errors get instead.
const ERROR_CONTENT_TYPE: &str = "application/json";

/// A layer for the SDK's HTTP+JSON router that
///
/// - refuses a request whose `A2A-Version` names a major version other than
///   1, an absent header counting as `0.3`, with `VersionNotSupportedError`
///   as a `google.rpc.Status` (A2A §11.6) and HTTP 400 (§5.4), exactly as
///   the SDK's JSON-RPC router checks it. SDK defect (a2a-server-lf 0.5.3,
///   `rest::rest_router`): only `jsonrpc::check_a2a_version` exists, so
///   HTTP+JSON requests of any version are served;
/// - gives the SDK's error responses the content type `application/json`.
///   SDK defect (a2a-server-lf 0.5.3, `rest::rest_error_response`): errors
///   are `application/problem+json`, while their body is the
///   `google.rpc.Status` of A2A §11.6, not RFC 9457 problem details; the A2A
///   TCK requires `application/json` (HTTP_JSON-ERR-001).
pub(crate) async fn rest_shim(request: Request, next: Next) -> Response {
    if let Err(err) = check_version(request.headers()) {
        return status_response(err);
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let problem = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next().unwrap_or("").trim() == PROBLEM_JSON);
    if problem {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(ERROR_CONTENT_TYPE));
    }
    response
}

/// The SDK's `check_a2a_version`, on the request's headers: the first value
/// that is text, else [`ABSENT_VERSION`], must have major version 1.
fn check_version(headers: &HeaderMap) -> Result<(), A2AError> {
    let requested = headers
        .get_all(VERSION_HEADER)
        .iter()
        .find_map(|value| value.to_str().ok())
        .unwrap_or(ABSENT_VERSION);
    let major = requested
        .trim()
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok());
    if major == Some(SUPPORTED_MAJOR_VERSION) {
        Ok(())
    } else {
        Err(A2AError::version_not_supported(requested))
    }
}

/// `err` as the SDK's HTTP+JSON binding renders an error: a
/// `google.rpc.Status` with a `google.rpc.ErrorInfo` detail.
fn status_response(err: A2AError) -> Response {
    let status =
        StatusCode::from_u16(err.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let metadata = HashMap::from([("timestamp", chrono::Utc::now().to_rfc3339())]);
    let body = json!({
        "error": {
            "code": status.as_u16(),
            "status": grpc_status(err.code),
            "message": err.message,
            "details": [{
                "@type": ERROR_INFO_TYPE,
                "reason": error_reason(err.code),
                "domain": PROTOCOL_DOMAIN,
                "metadata": metadata,
            }],
        }
    });
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(ERROR_CONTENT_TYPE));
    response
}

/// The `google.rpc.Code` name of the A2A errors this layer renders, as the
/// SDK names them.
fn grpc_status(code: i32) -> &'static str {
    match code {
        error_code::VERSION_NOT_SUPPORTED => "FAILED_PRECONDITION",
        _ => "INTERNAL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(VERSION_HEADER, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn versions_are_checked_as_the_sdk_checks_json_rpc() {
        for ok in [&["1.0"][..], &["1"], &[" 1.0 "], &["1.5"], &["1.0", "0.3"]] {
            assert!(check_version(&headers(ok)).is_ok(), "{ok:?}");
        }
        for refused in [&[][..], &["0.3"], &["2.0"], &[""], &["v1"], &["0.3", "1.0"]] {
            let err = check_version(&headers(refused)).unwrap_err();
            assert_eq!(err.code, error_code::VERSION_NOT_SUPPORTED, "{refused:?}");
        }
    }
}
