//! The agent's Agent Cards, signed, and the view of a peer's card that the
//! local gateway serves to standard clients.
//!
//! **Public card** ([`canonical_card`]): what the daemon serves, and the
//! services the inventory shows to everyone, at
//! `/.well-known/agent-card.json`. **Extended card** of a scope
//! ([`extended_card`]): the public card with the services visible in that
//! scope as well, which `GetExtendedAgentCard` returns. The scope is the
//! accepted grant's ([`Scope::for_grant`]), which the grant layer puts in
//! [`SCOPE_HEADER`]; nothing else a requester sends chooses it.
//!
//! [`CardSet`] builds and signs every card when the node starts: the public
//! one and one per scope the inventory names, each [normalized](normalize)
//! and signed with the agent's own key ([`sign_card`]), so what a request
//! gets depends on its scope only, never on who asks. A card that breaks a
//! card limit (`pals_core::card_jws::validate_card`: 32 KiB, 64 skills, 4 KiB
//! per string...) stops the node from starting, with an error that names the
//! limit and the card.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use a2a::{
    A2AError, AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentSkill,
    HttpAuthSecurityScheme, SecurityScheme, TRANSPORT_PROTOCOL_HTTP_JSON,
    TRANSPORT_PROTOCOL_JSONRPC,
};
use a2a_server::agent_card::AgentCardProducer;
use a2a_server::{ExtendedAgentCardResolver, ServiceParams};
use anyhow::{Context, anyhow};
use pals_core::card_jws::{CardError, CardHash, CardSigner, card_hash, normalize, sign_card};
use pals_core::grant::GRANT_SCHEMA;
use pals_core::identity::AgentId;
use pals_core::inventory::{
    INVENTORY_EXTENSION, INVENTORY_PROFILE, Inventory, Scope, Service, Visibility, capability_tag,
};
use serde_json::{Value, json};

use super::auth::{ASK_RESOURCE, SCOPE_HEADER};
use crate::config::Config;
use crate::discovery::DISCOVERY_ALPN;

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

/// The card's description when the configuration gives none: a card needs
/// one.
pub const DEFAULT_DESCRIPTION: &str = "A PALS agent";

/// The public card, [normalized](normalize) and unsigned: what
/// [`CardSet::build`] signs and the agent serves at
/// `/.well-known/agent-card.json`.
///
/// It declares only what the daemon serves: the JSON-RPC binding over Iroh
/// at `iroh://<agent id>/jsonrpc` ([`BINDING_URI_JSONRPC`]) and, second, the
/// HTTP+JSON binding over Iroh at `iroh://<agent id>/rest`
/// ([`BINDING_URI_HTTP_JSON`]), streaming, no push notifications, the
/// extended card, the optional [`IDEMPOTENCY_EXTENSION`] and, second, the
/// optional [`INVENTORY_EXTENSION`], whose `params` name its profile and the
/// ALPN of the discovery protocol that serves it, text and files of
/// any media type as input (`*/*`: a question comes with any files, sent as
/// blobs, and a part's media type never refuses it), and grants as bearer
/// tokens that must cover `skill:ask`. Its skills are `ask` and a skill for
/// each service of `inventory` whose visibility is
/// [`Public`](Visibility::Public), in the inventory's order: the service's
/// id, name and description, its capability tags ([`capability_tag`]) and
/// then its own tags, and its examples and input and output modes when it
/// has any. An empty `agent.description` becomes [`DEFAULT_DESCRIPTION`].
pub fn canonical_card(cfg: &Config, id: &AgentId, inventory: &Inventory) -> AgentCard {
    extended_card(cfg, id, inventory, &Scope::Public)
}

/// The extended card of `scope`, [normalized](normalize) and unsigned: the
/// [public card](canonical_card) whose skills are `ask` and every service of
/// `inventory` visible in `scope` ([`Visibility::visible_in`]), the public
/// ones included, in the inventory's order. The extended card of
/// [`Scope::Public`] is the public card.
pub fn extended_card(
    cfg: &Config,
    id: &AgentId,
    inventory: &Inventory,
    scope: &Scope,
) -> AgentCard {
    let idempotency = AgentExtension {
        uri: IDEMPOTENCY_EXTENSION.to_owned(),
        description: Some(
            "Opt-in idempotency by messageId: a retry with the same messageId and content \
             attaches to the first request's task or answer instead of running again."
                .to_owned(),
        ),
        // Optional; a normalized card leaves `required: false` out.
        required: None,
        params: None,
    };
    // Declared because the node serves `agentnet-discovery/0` on the ALPN it
    // names (`crate::node`); strings only, so that every SDK reads its
    // params alike.
    let inventory_extension = AgentExtension {
        uri: INVENTORY_EXTENSION.to_owned(),
        description: Some(
            "Signed inventory views and service queries over agentnet-discovery/0.".to_owned(),
        ),
        required: None,
        params: Some(HashMap::from([
            ("profile".to_owned(), json!(INVENTORY_PROFILE)),
            ("discoveryAlpn".to_owned(), json!(DISCOVERY_ALPN)),
        ])),
    };
    let capabilities = AgentCapabilities {
        streaming: Some(true),
        push_notifications: Some(false),
        extensions: Some(vec![idempotency, inventory_extension]),
        extended_agent_card: Some(true),
    };
    let skills = std::iter::once(ask_skill())
        .chain(
            inventory
                .services
                .iter()
                .filter(|service| service.visibility.visible_in(scope))
                .map(service_skill),
        )
        .collect();
    let grant_scheme = SecurityScheme::HttpAuth(HttpAuthSecurityScheme {
        scheme: "Bearer".to_owned(),
        description: None,
        bearer_format: Some(GRANT_SCHEMA.to_owned()),
    });
    let description = if cfg.agent.description.is_empty() {
        DEFAULT_DESCRIPTION.to_owned()
    } else {
        cfg.agent.description.clone()
    };
    let card = AgentCard {
        name: cfg.agent.name.clone(),
        description,
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
        skills,
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: Some(HashMap::from([(SECURITY_SCHEME.to_owned(), grant_scheme)])),
        // A scope list, never an empty one: the SDKs canonicalize an empty
        // list three different ways.
        security_requirements: Some(vec![HashMap::from([(
            SECURITY_SCHEME.to_owned(),
            vec![ASK_RESOURCE.to_owned()],
        )])]),
        signatures: None,
    };
    // A card that cannot be normalized (an empty `agent.name`) is left as
    // it is: signing it refuses it, with the reason.
    normalize(card.clone()).unwrap_or(card)
}

/// The card's own skill.
fn ask_skill() -> AgentSkill {
    AgentSkill {
        id: "ask".to_owned(),
        name: "Ask".to_owned(),
        description: "Answers a question with the context of one authorized project.".to_owned(),
        tags: vec!["ask".to_owned()],
        examples: None,
        input_modes: None,
        output_modes: None,
        security_requirements: None,
    }
}

/// The skill `service` becomes: its capability tags first, then its own
/// tags, each once; lists it does not have left out.
fn service_skill(service: &Service) -> AgentSkill {
    let mut tags: Vec<String> = Vec::new();
    // A loaded inventory has a tag for every capability (`Inventory::validate`).
    let capability_tags = service
        .capabilities
        .iter()
        .filter_map(|capability| capability_tag(capability).ok());
    for tag in capability_tags.chain(service.tags.iter().cloned()) {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    let listed = |items: &[String]| (!items.is_empty()).then(|| items.to_vec());
    AgentSkill {
        id: service.id.clone(),
        name: service.name.clone(),
        description: service.description.clone(),
        tags,
        examples: listed(&service.examples),
        input_modes: listed(&service.input_modes),
        output_modes: listed(&service.output_modes),
        security_requirements: None,
    }
}

/// A card as the agent signed it: the card, its JSON as served, and its
/// hash ([`card_hash`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SignedCard {
    pub card: AgentCard,
    pub json: Value,
    pub hash: CardHash,
}

/// Every card the agent serves, signed when the node starts: the public one
/// and the extended one of each scope the inventory names. Clones share
/// them.
///
/// It is the SDK's [`AgentCardProducer`], with the public card, and its
/// [`ExtendedAgentCardResolver`], with the card of the request's scope:
/// [`SCOPE_HEADER`], which the grant layer sets from the accepted grant.
#[derive(Debug, Clone)]
pub struct CardSet {
    inner: Arc<Cards>,
}

#[derive(Debug)]
struct Cards {
    public: SignedCard,
    /// The scopes whose extended card shows services the public one does
    /// not, by [`Scope::key`].
    scoped: BTreeMap<String, SignedCard>,
}

impl CardSet {
    /// Signs with `signer` the public card of `cfg` and `inventory`, and the
    /// extended card of every scope a service's visibility names: the
    /// project and task scopes of each project listed. A scope where no
    /// service but the public ones is visible gets no card of its own.
    ///
    /// Each card is validated as it is signed: an error that names the card
    /// and the limit it breaks when one does (larger than
    /// [`MAX_CARD_BYTES`](pals_core::card_jws::MAX_CARD_BYTES), more than
    /// [`MAX_CARD_SKILLS`](pals_core::card_jws::MAX_CARD_SKILLS) skills, a
    /// string longer than 4 KiB...), and when it cannot be normalized (an
    /// empty `agent.name`).
    pub fn build(cfg: &Config, signer: &CardSigner, inventory: &Inventory) -> anyhow::Result<Self> {
        let id: AgentId = signer
            .kid()
            .parse()
            .context("the card signer's kid is not an agent id")?;
        let public = sign(signer, canonical_card(cfg, &id, inventory))
            .map_err(|err| refused("the public Agent Card", err))?;
        let mut scoped = BTreeMap::new();
        for scope in named_scopes(inventory) {
            let own_services = inventory.services.iter().any(|service| {
                service.visibility != Visibility::Public && service.visibility.visible_in(&scope)
            });
            if !own_services {
                continue;
            }
            let key = scope.key();
            let card = sign(signer, extended_card(cfg, &id, inventory, &scope))
                .map_err(|err| refused(&format!("the extended Agent Card of scope {key}"), err))?;
            scoped.insert(key, card);
        }
        Ok(Self {
            inner: Arc::new(Cards { public, scoped }),
        })
    }

    /// The public card.
    pub fn public(&self) -> &SignedCard {
        &self.inner.public
    }

    /// The extended card of `scope`; the public card when the scope has no
    /// services of its own.
    pub fn for_scope(&self, scope: &Scope) -> &SignedCard {
        self.for_scope_key(&scope.key())
    }

    /// [`CardSet::for_scope`] of the scope whose [key](Scope::key) is `key`;
    /// the public card for any other text.
    pub fn for_scope_key(&self, key: &str) -> &SignedCard {
        self.inner.scoped.get(key).unwrap_or(&self.inner.public)
    }

    /// The hash of the card of every scope with a card of its own, by
    /// [`Scope::key`]; every other scope gets the public card.
    pub fn scoped_hashes(&self) -> BTreeMap<String, CardHash> {
        self.inner
            .scoped
            .iter()
            .map(|(key, card)| (key.clone(), card.hash.clone()))
            .collect()
    }
}

impl AgentCardProducer for CardSet {
    fn card(&self) -> AgentCard {
        self.public().card.clone()
    }
}

impl ExtendedAgentCardResolver for CardSet {
    /// The card of the request's scope, [`SCOPE_HEADER`]: the public card
    /// without one, or with more than one.
    fn resolve(&self, params: &ServiceParams) -> Result<AgentCard, A2AError> {
        let key = match params.get(SCOPE_HEADER).map(Vec::as_slice) {
            Some([key]) => key.as_str(),
            _ => "public",
        };
        Ok(self.for_scope_key(key).card.clone())
    }
}

/// `card` signed by `signer`, with its JSON and hash.
fn sign(signer: &CardSigner, card: AgentCard) -> Result<SignedCard, CardError> {
    let card = sign_card(signer, card)?;
    let json = serde_json::to_value(&card).expect("an AgentCard always serializes");
    let hash = card_hash(&json);
    Ok(SignedCard { card, json, hash })
}

/// Why `card` cannot be served, `err` naming the limit it breaks.
fn refused(card: &str, err: CardError) -> anyhow::Error {
    anyhow!(
        "{card} cannot be signed: {err}. A card holds the `ask` skill and one skill per service \
         it shows: show fewer services, or shorten them, in the inventory"
    )
}

/// The project and task scopes of every project a service's visibility
/// lists.
fn named_scopes(inventory: &Inventory) -> BTreeSet<Scope> {
    let mut scopes = BTreeSet::new();
    for service in &inventory.services {
        if let Visibility::Project(projects) | Visibility::Task(projects) = &service.visibility {
            for project in projects {
                scopes.insert(Scope::Project(project.clone()));
                scopes.insert(Scope::Task(project.clone()));
            }
        }
    }
    scopes
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
        let mut card = canonical_card(&cfg, &peer, &Inventory::default());
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
