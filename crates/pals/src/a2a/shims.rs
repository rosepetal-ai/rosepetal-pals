//! What the SDK's bindings lack for A2A conformance, made up for in front
//! of their routers: [`rest_shim`] for HTTP+JSON, [`jsonrpc_shim`] for
//! JSON-RPC, and [`echo_extensions`] for both.

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

use pals_core::inventory::INVENTORY_EXTENSION;

use super::IDEMPOTENCY_EXTENSION;
use super::guard::names_extension;

/// The service parameter that names the client's protocol version.
const VERSION_HEADER: &str = "a2a-version";
/// The service parameter that lists the extensions a request activates, and
/// a response the extensions it activated.
const EXTENSIONS_HEADER: &str = "a2a-extensions";
/// The request parameter a client may send the version in instead
/// (A2A §3.6.1), its key matched ignoring ASCII case (§3.2.6).
const VERSION_PARAM: &str = "a2a-version";
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
/// - refuses a request whose `A2A-Version` ([`requested_version`]: the
///   header, else the request parameter) names a major version other than
///   1, an absent one counting as `0.3`, with `VersionNotSupportedError` as
///   a `google.rpc.Status` (A2A §11.6) and HTTP 400 (§5.4), as the SDK's
///   JSON-RPC router checks it. SDK defect (a2a-server-lf 0.5.3,
///   `rest::rest_router`): only `jsonrpc::check_a2a_version` exists, so
///   HTTP+JSON requests of any version are served;
/// - gives the SDK's error responses the content type `application/json`.
///   SDK defect (a2a-server-lf 0.5.3, `rest::rest_error_response`): errors
///   are `application/problem+json`, while their body is the
///   `google.rpc.Status` of A2A §11.6, not RFC 9457 problem details; the A2A
///   TCK requires `application/json` (HTTP_JSON-ERR-001).
pub(crate) async fn rest_shim(request: Request, next: Next) -> Response {
    let requested = requested_version(request.headers(), request.uri().query());
    if let Err(err) = check_version(requested.as_deref()) {
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

/// A layer for the SDK's JSON-RPC router that gives a request with no
/// `A2A-Version` header the version of its `A2A-Version` request parameter,
/// as a header, for the SDK to check (A2A §3.6.1). SDK defect
/// (a2a-server-lf 0.5.3, `jsonrpc::check_a2a_version`): it reads only the
/// header, so a version sent as a request parameter counts as `0.3`.
pub(crate) async fn jsonrpc_shim(mut request: Request, next: Next) -> Response {
    if header_version(request.headers()).is_none()
        && let Some(version) = query_version(request.uri().query())
        && let Ok(value) = HeaderValue::from_str(&version)
    {
        request.headers_mut().insert(VERSION_HEADER, value);
    }
    next.run(request).await
}

/// A layer for both bindings' routers: the response to a request that
/// activates the [`IDEMPOTENCY_EXTENSION`], the [`INVENTORY_EXTENSION`] or
/// both lists them, in that order, in its `A2A-Extensions` header, the
/// extensions activated for the request, as A2A asks a server to (SHOULD).
/// A request that activates only idempotency gets it alone, as in
/// milestone 1; one that activates neither gets no header.
pub(crate) async fn echo_extensions(request: Request, next: Next) -> Response {
    let echoed = {
        let values = || {
            request
                .headers()
                .get_all(EXTENSIONS_HEADER)
                .iter()
                .filter_map(|value| value.to_str().ok())
        };
        let activated: Vec<&'static str> = [IDEMPOTENCY_EXTENSION, INVENTORY_EXTENSION]
            .into_iter()
            .filter(|uri| names_extension(values(), uri))
            .collect();
        activated.join(", ")
    };
    let mut response = next.run(request).await;
    if !echoed.is_empty() {
        let value = HeaderValue::from_str(&echoed).expect("extension URIs are header values");
        response.headers_mut().insert(EXTENSIONS_HEADER, value);
    }
    response
}

/// The protocol version a request names: its first `A2A-Version` header
/// that is text, else its first `A2A-Version` request parameter
/// ([`query_version`]); `None` when it names none.
fn requested_version(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    header_version(headers)
        .map(str::to_owned)
        .or_else(|| query_version(query))
}

/// The first `A2A-Version` header of `headers` that is text.
fn header_version(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(VERSION_HEADER)
        .iter()
        .find_map(|value| value.to_str().ok())
}

/// The value of the first [`VERSION_PARAM`] of a query string, its key
/// matched ignoring ASCII case, both percent-decoded (`+` is a space).
fn query_version(query: Option<&str>) -> Option<String> {
    let decode = |text: &str| {
        percent_encoding::percent_decode_str(&text.replace('+', " "))
            .decode_utf8()
            .ok()
            .map(|text| text.into_owned())
    };
    query?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        decode(key)?
            .eq_ignore_ascii_case(VERSION_PARAM)
            .then(|| decode(value))?
    })
}

/// The SDK's `check_a2a_version` on the version a request names, `None`
/// meaning [`ABSENT_VERSION`]: it must have major version 1.
fn check_version(requested: Option<&str>) -> Result<(), A2AError> {
    let requested = requested.unwrap_or(ABSENT_VERSION);
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

    fn check(values: &[&str], query: Option<&str>) -> Result<(), A2AError> {
        check_version(requested_version(&headers(values), query).as_deref())
    }

    #[test]
    fn versions_are_checked_as_the_sdk_checks_json_rpc() {
        for ok in [&["1.0"][..], &["1"], &[" 1.0 "], &["1.5"], &["1.0", "0.3"]] {
            assert!(check(ok, None).is_ok(), "{ok:?}");
        }
        for refused in [&[][..], &["0.3"], &["2.0"], &[""], &["v1"], &["0.3", "1.0"]] {
            let err = check(refused, None).unwrap_err();
            assert_eq!(err.code, error_code::VERSION_NOT_SUPPORTED, "{refused:?}");
        }
    }

    #[test]
    fn without_a_header_the_version_may_be_a_request_parameter() {
        for ok in [
            "A2A-Version=1.0",
            "a2a-version=1.0",
            "x=1&A2A-VERSION=1",
            "A2A%2DVersion=1%2E0",
            "a2a-version=1.0&a2a-version=0.3",
        ] {
            assert!(check(&[], Some(ok)).is_ok(), "{ok}");
        }
        for refused in [
            "A2A-Version=0.3",
            "A2A-Version",
            "A2A-Version=",
            "version=1.0",
            "A2A_Version=1.0",
            "",
        ] {
            assert!(check(&[], Some(refused)).is_err(), "{refused}");
        }
        // A header wins over the parameter.
        assert!(check(&["0.3"], Some("A2A-Version=1.0")).is_err());
        assert!(check(&["1.0"], Some("A2A-Version=0.3")).is_ok());
    }
}
