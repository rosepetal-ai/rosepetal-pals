//! The agent's public Agent Card, and the view of a peer's card that the
//! local gateway serves to standard clients.

use std::collections::HashMap;

use a2a::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentSkill,
    HttpAuthSecurityScheme, SecurityScheme, TRANSPORT_PROTOCOL_HTTP_JSON,
    TRANSPORT_PROTOCOL_JSONRPC,
};
use pals_core::grant::GRANT_SCHEMA;
use pals_core::identity::AgentId;

use crate::config::Config;

/// The custom protocol binding (A2A §5.8) with which peers reach the
/// agent's JSON-RPC interface, at `iroh://<agent id>/jsonrpc`: the standard
/// JSON-RPC binding, unchanged, with HTTP/1.1 carried by an Iroh stream
/// (`docs/bindings/http-over-iroh-v1.md`).
pub const BINDING_URI_JSONRPC: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1/jsonrpc";

/// The custom protocol binding with which peers reach the agent's HTTP+JSON
/// interface, at `iroh://<agent id>/rest`: the standard HTTP+JSON binding,
/// unchanged, carried the same way.
pub const BINDING_URI_HTTP_JSON: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1/http-json";

/// The A2A extension that makes `SendMessage` and `SendStreamingMessage`
/// idempotent by `messageId` (`docs/extensions/idempotency-v1.md`). A
/// request activates it with the `A2A-Extensions` service parameter;
/// without it the agent treats every message as new, as the reference SDKs
/// do.
pub const IDEMPOTENCY_EXTENSION: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/extensions/idempotency/v1";

/// The name of the card's security scheme: a grant token presented as an
/// HTTP bearer token.
pub const SECURITY_SCHEME: &str = "agentnetGrant";

/// The card the agent serves at `/.well-known/agent-card.json`, unsigned.
///
/// It declares only what the daemon serves: the JSON-RPC binding over Iroh
/// at `iroh://<agent id>/jsonrpc` ([`BINDING_URI_JSONRPC`]) and, second, the
/// HTTP+JSON binding over Iroh at `iroh://<agent id>/rest`
/// ([`BINDING_URI_HTTP_JSON`]), streaming, no push notifications, no
/// extended card, the optional [`IDEMPOTENCY_EXTENSION`], text and files of
/// any media type as input (`*/*`: a question comes with any files, sent as
/// blobs, and a part's media type never refuses it), the `ask` skill, and
/// grants as bearer tokens.
pub fn canonical_card(cfg: &Config, id: &AgentId) -> AgentCard {
    let idempotency = AgentExtension {
        uri: IDEMPOTENCY_EXTENSION.to_owned(),
        description: Some(
            "Opt-in idempotency by messageId: a retry with the same messageId and content \
             attaches to the first request's task or answer instead of running again."
                .to_owned(),
        ),
        required: Some(false),
        params: None,
    };
    let capabilities = AgentCapabilities {
        streaming: Some(true),
        push_notifications: Some(false),
        extensions: Some(vec![idempotency]),
        extended_agent_card: Some(false),
    };
    let ask = AgentSkill {
        id: "ask".to_owned(),
        name: "Ask".to_owned(),
        description: "Answers a question with the context of one authorized project.".to_owned(),
        tags: vec!["ask".to_owned()],
        examples: None,
        input_modes: None,
        output_modes: None,
        security_requirements: None,
    };
    let grant_scheme = SecurityScheme::HttpAuth(HttpAuthSecurityScheme {
        scheme: "Bearer".to_owned(),
        description: None,
        bearer_format: Some(GRANT_SCHEMA.to_owned()),
    });
    AgentCard {
        name: cfg.agent.name.clone(),
        description: cfg.agent.description.clone(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        // In order of preference. `new` sets the protocol version this SDK
        // implements, 1.0.
        supported_interfaces: vec![
            AgentInterface::new(format!("iroh://{id}/jsonrpc"), BINDING_URI_JSONRPC),
            AgentInterface::new(format!("iroh://{id}/rest"), BINDING_URI_HTTP_JSON),
        ],
        capabilities,
        default_input_modes: vec!["text/plain".to_owned(), "*/*".to_owned()],
        default_output_modes: vec![
            "text/plain".to_owned(),
            "application/octet-stream".to_owned(),
        ],
        skills: vec![ask],
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: Some(HashMap::from([(SECURITY_SCHEME.to_owned(), grant_scheme)])),
        security_requirements: Some(vec![HashMap::from([(
            SECURITY_SCHEME.to_owned(),
            Vec::new(),
        )])]),
        signatures: None,
    }
}

/// `card`, a card of `peer`, as the local gateway at `base_url` serves it:
/// the same card, except that it is reached through the gateway with the
/// standard bindings, JSON-RPC at `<base_url>/peers/<peer>/jsonrpc` and
/// HTTP+JSON at `<base_url>/peers/<peer>/rest` (protocol version 1.0), and
/// that it declares no security, because the gateway presents the
/// credential: no security schemes, no security requirements, on the card or
/// on its skills, and no signatures, which covered the original.
pub fn gateway_view(card: &AgentCard, base_url: &str, peer: &AgentId) -> AgentCard {
    let base = base_url.trim_end_matches('/');
    let mut view = card.clone();
    // `new` sets the protocol version this SDK implements, 1.0.
    view.supported_interfaces = vec![
        AgentInterface::new(
            format!("{base}/peers/{peer}/jsonrpc"),
            TRANSPORT_PROTOCOL_JSONRPC,
        ),
        AgentInterface::new(
            format!("{base}/peers/{peer}/rest"),
            TRANSPORT_PROTOCOL_HTTP_JSON,
        ),
    ];
    view.security_schemes = None;
    view.security_requirements = None;
    view.signatures = None;
    for skill in &mut view.skills {
        skill.security_requirements = None;
    }
    view
}

#[cfg(test)]
mod tests {
    use pals_core::identity::AgentKey;

    use super::*;

    #[test]
    fn gateway_view_replaces_interfaces_and_drops_security_only() {
        let peer = AgentKey::generate().id();
        let cfg: Config = toml::from_str("[agent]\nname = \"bob\"\n").unwrap();
        let mut card = canonical_card(&cfg, &peer);
        card.skills[0].security_requirements = card.security_requirements.clone();

        let view = gateway_view(&card, "http://127.0.0.1:7420/", &peer);
        assert_eq!(
            view.supported_interfaces,
            vec![
                AgentInterface {
                    url: format!("http://127.0.0.1:7420/peers/{peer}/jsonrpc"),
                    protocol_binding: "JSONRPC".to_owned(),
                    protocol_version: "1.0".to_owned(),
                    tenant: None,
                },
                AgentInterface {
                    url: format!("http://127.0.0.1:7420/peers/{peer}/rest"),
                    protocol_binding: "HTTP+JSON".to_owned(),
                    protocol_version: "1.0".to_owned(),
                    tenant: None,
                },
            ]
        );
        assert_eq!(view.security_schemes, None);
        assert_eq!(view.security_requirements, None);
        assert_eq!(view.signatures, None);
        assert_eq!(view.skills[0].security_requirements, None);

        // Everything else is the card as it came.
        let mut back = view.clone();
        back.supported_interfaces = card.supported_interfaces.clone();
        back.security_schemes = card.security_schemes.clone();
        back.security_requirements = card.security_requirements.clone();
        back.skills[0].security_requirements = card.skills[0].security_requirements.clone();
        assert_eq!(back, card);
    }
}
