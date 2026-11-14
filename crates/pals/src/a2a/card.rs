//! The agent's public Agent Card.

use std::collections::HashMap;

use a2a::{
    AgentCapabilities, AgentCard, AgentInterface, AgentSkill, HttpAuthSecurityScheme,
    SecurityScheme,
};
use pals_core::grant::GRANT_SCHEMA;
use pals_core::identity::AgentId;

use crate::config::Config;

/// The custom protocol binding peers reach the agent with (A2A §5.8): the
/// standard JSON-RPC binding, unchanged, with HTTP/1.1 carried by an Iroh
/// stream.
pub const BINDING_URI: &str =
    "https://github.com/rosepetal-ai/rosepetal-pals/bindings/http-over-iroh/v1";

/// The name of the card's security scheme: a grant token presented as an
/// HTTP bearer token.
pub const SECURITY_SCHEME: &str = "agentnetGrant";

/// The card the agent serves at `/.well-known/agent-card.json`, unsigned.
///
/// It declares only what the daemon serves: the JSON-RPC binding over Iroh
/// at `iroh://<agent id>/jsonrpc`, streaming, no push notifications, no
/// extended card, the `ask` skill, and grants as bearer tokens.
pub fn canonical_card(cfg: &Config, id: &AgentId) -> AgentCard {
    let capabilities = AgentCapabilities {
        streaming: Some(true),
        push_notifications: Some(false),
        extensions: None,
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
        // `new` sets the protocol version this SDK implements, 1.0.
        supported_interfaces: vec![AgentInterface::new(
            format!("iroh://{id}/jsonrpc"),
            BINDING_URI,
        )],
        capabilities,
        default_input_modes: vec!["text/plain".to_owned()],
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
