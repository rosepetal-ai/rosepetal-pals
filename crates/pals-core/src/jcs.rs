//! Canonical JSON (JCS, RFC 8785) and Ed25519 signatures over it, separated by
//! domain.
//!
//! A signature covers `domain || JCS(value)`. Each kind of signed object has
//! its own domain (`agentnet.grant/1\n`, `agentnet.event/1\n`), so a signature
//! made for one kind is never valid for another. Signatures travel as base64url
//! without padding.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use crate::identity::{AgentId, AgentKey};

/// The RFC 8785 canonical form of `value`.
pub fn canonical(value: &Value) -> Vec<u8> {
    // A `Value` holds only finite numbers and unique string keys, the only
    // inputs the canonicalizer refuses, and writing to a `Vec` cannot fail.
    serde_json_canonicalizer::to_vec(value).expect("a serde_json::Value always canonicalizes")
}

/// `key`'s signature of `domain || canonical(value)`, in base64url without
/// padding.
pub fn sign_domain(key: &AgentKey, domain: &[u8], value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(key.sign(&signed_bytes(domain, value)))
}

/// Whether `sig_b64` is `id`'s signature of `domain || canonical(value)`.
///
/// Only the strict base64url form without padding is accepted.
pub fn verify_domain(id: &AgentId, domain: &[u8], value: &Value, sig_b64: &str) -> bool {
    let Ok(sig) = URL_SAFE_NO_PAD.decode(sig_b64) else {
        return false;
    };
    let Ok(sig) = <[u8; 64]>::try_from(sig.as_slice()) else {
        return false;
    };
    id.verify(&signed_bytes(domain, value), &sig)
}

fn signed_bytes(domain: &[u8], value: &Value) -> Vec<u8> {
    let mut bytes = domain.to_vec();
    bytes.extend_from_slice(&canonical(value));
    bytes
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_follows_rfc_8785() {
        // Keys sorted by UTF-16 code units, no whitespace, numbers in their
        // ECMAScript form, only the required escapes.
        let value: Value = serde_json::from_str(
            r#"{"numbers": [333333333.33333329, 1E30, 4.50, 2e-3, 0.000000000000000000000000001],
                "string": "\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
                "literals": [null, true, false]}"#,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(canonical(&value)).unwrap(),
            r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"€$\u000f\nA'B\"\\\\\"/"}"#
        );
    }

    #[test]
    fn signature_is_bound_to_its_domain_and_value() {
        let key = AgentKey::generate();
        let value = json!({"b": 1, "a": "x"});
        let sig = sign_domain(&key, b"agentnet.test/1\n", &value);
        assert!(verify_domain(&key.id(), b"agentnet.test/1\n", &value, &sig));
        // Key order does not change the canonical form.
        let reordered: Value = serde_json::from_str(r#"{"a":"x","b":1}"#).unwrap();
        assert!(verify_domain(
            &key.id(),
            b"agentnet.test/1\n",
            &reordered,
            &sig
        ));

        assert!(!verify_domain(
            &key.id(),
            b"agentnet.other/1\n",
            &value,
            &sig
        ));
        assert!(!verify_domain(
            &key.id(),
            b"agentnet.test/1\n",
            &json!({"b": 2, "a": "x"}),
            &sig
        ));
        assert!(!verify_domain(
            &AgentKey::generate().id(),
            b"agentnet.test/1\n",
            &value,
            &sig
        ));
        assert!(!verify_domain(
            &key.id(),
            b"agentnet.test/1\n",
            &value,
            &format!("{sig}=")
        ));
        assert!(!verify_domain(
            &key.id(),
            b"agentnet.test/1\n",
            &value,
            "not base64!"
        ));
        assert!(!verify_domain(
            &key.id(),
            b"agentnet.test/1\n",
            &value,
            &sig[..40]
        ));
    }
}
