//! `baseline`: contacts only, the situation of hito 2.
//!
//! The node knows only the contacts `--bootstrap` gives it, and finds a
//! provider only among them: its search asks each contact, over HTTP on the
//! contact's listen port (`POST /lookup {"capability"}`), whether it
//! announces the capability, and a contact that does answers with its own
//! two records, `{"contact", "announce"}`, each the JSON of its signed form;
//! one that does not, `404`. In the topology no client has a provider as a
//! contact, so without prior contact it finds no one: scenario S1 is
//! `by-design`, and the scenarios about how a DHT copes with change do not
//! apply ([`Baseline::DECLARED`]). It stores nothing for anyone.
//!
//! Like the daemon of hito 2, it holds an Iroh endpoint with the agent's
//! key, on UDP at the listen port, with no relay, port mapping or address
//! lookup, so that its idle costs (scenario S6) are the reference the
//! candidates add theirs to. The endpoint accepts no protocol: the contact
//! records name it, and nothing dials it.
//!
//! Its `bootstrap_addr` is every address of the node, `ip:port` joined by
//! commas: a router is on both networks, and a node on one reaches it only
//! at that network's address. The search tries a contact's addresses in
//! order, and an address on a network the node is not on fails at once
//! (internal networks give no default route).

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::{Endpoint, RelayMode, SecretKey};
use pals_core::identity::AgentKey;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use serde::{Deserialize, Serialize};
use spike_common::{
    Backend, BackendStats, Declared, DeclaredStatus, NodeConfig, RawCandidate, RawSearch,
    local_addrs,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// The route a contact answers lookups on.
const LOOKUP_PATH: &str = "/lookup";
/// How long a connection to one address may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a contact may take to answer, once connected.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest answer read: two records of at most 4 KiB, escaped.
const MAX_ANSWER_BYTES: u64 = 64 * 1024;

/// What the node announces, if anything.
#[derive(Debug, Clone)]
struct Own {
    capabilities: Vec<String>,
    records: Records,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Records {
    contact: String,
    announce: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    capability: String,
}

type Shared = Arc<RwLock<Option<Own>>>;

struct Baseline {
    /// Each contact's addresses, from `--bootstrap`.
    contacts: Vec<Vec<SocketAddr>>,
    own: Shared,
    bootstrap_addr: String,
    /// Held so that it stays bound.
    _endpoint: Endpoint,
}

impl Backend for Baseline {
    const DECLARED: &'static [Declared] = &[
        Declared {
            scenario: "S1",
            status: DeclaredStatus::ByDesign,
            notes: "Solo contactos, como en el hito 2: el nodo solo pregunta a los contactos que ya tiene, ninguno es proveedor, y sin contacto previo no encuentra a nadie.",
        },
        Declared {
            scenario: "S2",
            status: DeclaredStatus::NotApplicable,
            notes: "Sin descubrimiento no hay búsqueda que siga funcionando al caer el arranque original: los clientes no encontraban a nadie antes del corte.",
        },
        Declared {
            scenario: "S3",
            status: DeclaredStatus::NotApplicable,
            notes: "Los routers no guardan registros de nadie: no hay anuncio que caduque.",
        },
        Declared {
            scenario: "S4",
            status: DeclaredStatus::NotApplicable,
            notes: "Sin DHT, la caída de routers no cambia una cobertura que ya es nula.",
        },
        Declared {
            scenario: "S5",
            status: DeclaredStatus::NotApplicable,
            notes: "Los routers no aceptan registros ajenos, así que no hay nada que rechazar, y a los clientes no les llega el anuncio del nodo rogue. Lo que sí se comprueba es que el cliente rechaza su contacto (prueba verify_rejects_a_tampered_contact).",
        },
        Declared {
            scenario: "S7",
            status: DeclaredStatus::NotApplicable,
            notes: "Sin descubrimiento, ningún cliente resuelve a un proveedor que no es contacto suyo, ni antes ni después de que cambie de red.",
        },
    ];

    async fn start(cfg: &NodeConfig, key: &AgentKey) -> anyhow::Result<Self> {
        let contacts = cfg
            .bootstrap
            .iter()
            .map(|contact| parse_contact(contact))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let own = Shared::default();
        let listener = TcpListener::bind(cfg.listen)
            .await
            .with_context(|| format!("cannot listen on {}", cfg.listen))?;
        let app = Router::new()
            .route(LOOKUP_PATH, post(lookup))
            .with_state(Arc::clone(&own));
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::error!("the lookup server stopped: {error}");
            }
        });
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr(cfg.listen)
            .context("invalid listen address")?
            .bind()
            .await
            .context("cannot bind the Iroh endpoint")?;
        let bootstrap_addr = local_addrs(cfg.listen)
            .iter()
            .map(SocketAddr::to_string)
            .collect::<Vec<_>>()
            .join(",");
        tracing::info!(%bootstrap_addr, contacts = contacts.len(), "baseline started");
        Ok(Self {
            contacts,
            own,
            bootstrap_addr,
            _endpoint: endpoint,
        })
    }

    async fn announce(
        &mut self,
        capabilities: &[String],
        contact: &Signed<ContactRecord>,
        announce: &Signed<AnnounceRecord>,
    ) -> anyhow::Result<()> {
        // Nothing to renew: the node answers with its records while it runs.
        let records = Records {
            contact: String::from_utf8(contact.to_json())?,
            announce: String::from_utf8(announce.to_json())?,
        };
        *self.own.write().unwrap_or_else(|e| e.into_inner()) = Some(Own {
            capabilities: capabilities.to_vec(),
            records,
        });
        Ok(())
    }

    async fn unannounce(&mut self) -> anyhow::Result<()> {
        *self.own.write().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }

    async fn search(&mut self, capability: &str, _limit: usize) -> anyhow::Result<RawSearch> {
        let body = serde_json::to_vec(&Lookup {
            capability: capability.to_owned(),
        })?;
        let mut found = RawSearch {
            candidates: Vec::new(),
            requests: Some(0),
        };
        for addrs in &self.contacts {
            let (requests, candidate) = ask(addrs, &body).await;
            found.requests = found.requests.map(|n| n + requests);
            found.candidates.extend(candidate);
        }
        Ok(found)
    }

    fn stats(&self) -> BackendStats {
        BackendStats {
            // The lookups are one request per connection, closed after it.
            connections: 0,
            routing_peers: self.contacts.len() as u64,
            bootstrap_addr: Some(self.bootstrap_addr.clone()),
            ..BackendStats::default()
        }
    }
}

/// A `--bootstrap` of the baseline: `ip:port[,ip:port…]`.
fn parse_contact(text: &str) -> anyhow::Result<Vec<SocketAddr>> {
    let addrs = text
        .split(',')
        .map(|addr| addr.trim().parse::<SocketAddr>())
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("a baseline contact is ip:port[,ip:port…], not {text:?}"))?;
    ensure!(!addrs.is_empty(), "an empty contact");
    Ok(addrs)
}

/// Asks one contact, at the first of its addresses that takes a
/// connection: the requests sent (0 or 1) and the candidate, if it
/// announces the capability.
async fn ask(addrs: &[SocketAddr], body: &[u8]) -> (u64, Option<RawCandidate>) {
    for &addr in addrs {
        let Ok(Ok(stream)) = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await else {
            continue;
        };
        return match exchange(stream, addr, body).await {
            Ok(candidate) => (1, candidate),
            Err(error) => {
                tracing::warn!(%addr, "lookup failed: {error:#}");
                (1, None)
            }
        };
    }
    (0, None)
}

/// One `POST /lookup` over `stream`, HTTP/1.1 with `connection: close`.
async fn exchange(
    mut stream: TcpStream,
    addr: SocketAddr,
    body: &[u8],
) -> anyhow::Result<Option<RawCandidate>> {
    let head = format!(
        "POST {LOOKUP_PATH} HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let answer = timeout(ANSWER_TIMEOUT, async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body).await?;
        let mut answer = Vec::new();
        (&mut stream)
            .take(MAX_ANSWER_BYTES)
            .read_to_end(&mut answer)
            .await?;
        Ok::<_, std::io::Error>(answer)
    })
    .await
    .context("no answer in time")??;
    let (status, body) = parse_answer(&answer)?;
    match status {
        200 => {
            let records: Records = serde_json::from_slice(body)?;
            Ok(Some(RawCandidate {
                contact: Some(records.contact.into_bytes()),
                announce: Some(records.announce.into_bytes()),
            }))
        }
        404 => Ok(None),
        other => bail!("status {other}"),
    }
}

/// The status and body of an HTTP/1.1 answer with a `content-length`.
fn parse_answer(answer: &[u8]) -> anyhow::Result<(u16, &[u8])> {
    let end = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("an answer with no end of head")?;
    let head = std::str::from_utf8(&answer[..end])?.to_ascii_lowercase();
    ensure!(!head.contains("transfer-encoding:"), "a chunked answer");
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .context("an answer with no status")?;
    Ok((status, &answer[end + 4..]))
}

/// A contact's side: its own records, when it announces `capability`.
async fn lookup(State(own): State<Shared>, Json(lookup): Json<Lookup>) -> Response {
    let own = own.read().unwrap_or_else(|e| e.into_inner()).clone();
    match own {
        Some(own) if own.capabilities.contains(&lookup.capability) => {
            Json(own.records).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    spike_common::run_node::<Baseline>().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contact_lists_its_addresses() {
        let addrs = parse_contact("172.27.0.2:4433,172.28.0.2:4433").unwrap();
        assert_eq!(addrs.len(), 2);
        assert!(parse_contact("").is_err());
        assert!(parse_contact("r1").is_err());
    }

    #[test]
    fn an_answer_has_a_status_and_a_body() {
        let answer = b"HTTP/1.1 404 Not Found\r\ncontent-length: 2\r\n\r\n{}";
        assert_eq!(parse_answer(answer).unwrap(), (404, &b"{}"[..]));
        assert!(parse_answer(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n").is_err());
    }
}
