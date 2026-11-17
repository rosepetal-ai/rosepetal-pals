//! The DHT in the daemon: real nodes on loopback, with no relay, each with
//! its Kademlia swarm over its own Iroh endpoint (`agentnet-kad/0`), as
//! routers or clients, entering the network through other nodes' tickets
//! or, with those down, through their saved contacts; what they refuse
//! (connections and streams beyond their limits, idle streams), what their
//! tickets and `pals dht status` say, and the `[dht]` sections refused.
//!
//! The tests wait for the state they need ([`wait_for`]) and never for time
//! to pass; a deadline only fails them. What follows `libp2p-kad`'s own
//! times (the idle connection timeout) runs with short times from
//! `NodeOptions::dht_timing`.

mod common;

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::Path;
use std::time::{Duration, Instant};

use common::{
    TestNode, gateway_request, key_in, loopback_endpoint, now_unix, shut_down, spawn_dht_node,
    spawn_dht_node_in, spawn_dht_node_with, spawn_node,
};
use iroh::endpoint::{Connection, ReadError, RecvStream, SendStream, VarInt};
use iroh::{EndpointAddr, TransportAddr};
use iroh_tickets::endpoint::EndpointTicket;
use libp2p::futures::StreamExt;
use libp2p::kad::store::MemoryStore;
use libp2p::kad::{self, Mode, Quorum, Record};
use libp2p::swarm::{self, SwarmEvent};
use libp2p::{PeerId, Swarm, Transport};
use pals::config::{CONFIG_FILE, Config, DhtCfg, DhtMode, WorkerCfg};
use pals::dht::keys::{announce_key, peer_id, provider_key};
use pals::dht::swarm::KAD_PROTOCOL;
use pals::dht::transport::{
    ALPN_KAD, AddressBook, IrohTransport, KadQuotas, REFUSED, located_nowhere, multiaddr_of,
};
use pals::dht::{DhtStatus, DhtTiming};
use pals::node::{Node, NodeOptions, STORE_FILE};
use pals_core::card_jws::CardHash;
use pals_core::clock::SystemClock;
use pals_core::grant::format_timestamp;
use pals_core::identity::{AgentId, AgentKey};
use pals_core::records::{ANNOUNCE_SCHEMA, AnnounceRecord, RECORD_TTL_S};
use pals_core::signed::Signed;
use pals_core::store::{Contact, Store};
use serde_json::Value;

/// How long a test waits for the network to reach the state it needs: past
/// it, the test fails.
const CONVERGES: Duration = Duration::from_secs(60);

/// What multistream-select 1.0 says to open `/agentnet/kad/1.0.0` on a
/// stream, and what a node that serves it answers: the header and the
/// protocol, each prefixed by its length.
const NEGOTIATION: &[u8] = b"\x13/multistream/1.0.0\n\x14/agentnet/kad/1.0.0\n";

/// The DHT's times of the configuration's defaults.
fn timing() -> DhtTiming {
    DhtTiming::from_config(&DhtCfg::default())
}

/// [`timing`], with connections and incoming streams idle after `idle`.
fn idle_after(idle: Duration) -> DhtTiming {
    DhtTiming {
        idle_timeout: idle,
        ..timing()
    }
}

/// `node`'s DHT status now.
async fn status(node: &TestNode) -> DhtStatus {
    node.node
        .dht()
        .expect("the node's DHT is on")
        .status()
        .await
        .expect("the node's DHT runs")
}

/// Polls `node`'s status until `done`, and returns it; fails, saying
/// `what`, after [`CONVERGES`].
async fn wait_for(node: &TestNode, what: &str, done: impl Fn(&DhtStatus) -> bool) -> DhtStatus {
    let deadline = Instant::now() + CONVERGES;
    loop {
        let now = status(node).await;
        if done(&now) {
            return now;
        }
        assert!(Instant::now() < deadline, "{what}: {now:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Waits until `node` has `peers` routers in its routing table.
async fn wait_for_routers(node: &TestNode, peers: usize) -> DhtStatus {
    let what = format!("{} routers in the table", peers);
    wait_for(node, &what, |s| s.routing_peers == peers).await
}

/// A UDP port of 127.0.0.1 that was free a moment ago.
fn free_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Runs `pals --home <home> <args>`, off the async threads: its exit code,
/// stdout and stderr.
async fn pals(home: &Path, args: &[&str]) -> (i32, String, String) {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("pals");
    cmd.env_remove("PALS_HOME")
        .env_remove("RUST_LOG")
        .arg("--home")
        .arg(home)
        .args(args);
    let output = tokio::task::spawn_blocking(move || cmd.output().expect("run pals"))
        .await
        .unwrap();
    (
        output.status.code().expect("an exit code"),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// Points a configuration in `node`'s data directory at its gateway, so
/// that the `pals` command finds its key and its running daemon.
fn cli_home(node: &TestNode) -> &Path {
    let listen = node.gateway_url().replace("http://", "");
    std::fs::write(
        node.dir.path().join(CONFIG_FILE),
        format!("[gateway]\nlisten = \"{listen}\"\n\n[net]\nrelay = false\n"),
    )
    .unwrap();
    node.dir.path()
}

/// `kad-v0.json`, the frozen vector of the DHT's names.
fn kad_vector() -> Value {
    let path = format!(
        "{}/../pals-core/tests/vectors/kad-v0.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Opens a stream on `connection` and asks for `/agentnet/kad/1.0.0` on it.
async fn open_kad_stream(connection: &Connection) -> (SendStream, RecvStream) {
    let (mut send, recv) = connection.open_bi().await.expect("open a stream");
    send.write_all(NEGOTIATION)
        .await
        .expect("write the negotiation");
    (send, recv)
}

/// Reads what the other side answers to [`NEGOTIATION`].
async fn negotiated(recv: &mut RecvStream) -> Result<Vec<u8>, ReadError> {
    let mut answer = vec![0; NEGOTIATION.len()];
    let read = tokio::time::timeout(CONVERGES, recv.read_exact(&mut answer))
        .await
        .expect("an answer or a reset");
    match read {
        Ok(()) => Ok(answer),
        Err(iroh::endpoint::ReadExactError::ReadError(error)) => Err(error),
        Err(error) => panic!("the stream ended early: {error}"),
    }
}

#[test]
fn kad_protocol_is_the_vectors() {
    assert_eq!(
        KAD_PROTOCOL.as_ref(),
        kad_vector()["protocol"].as_str().unwrap()
    );
    assert_eq!(ALPN_KAD, kad_vector()["alpn"].as_str().unwrap().as_bytes());
}

#[tokio::test]
async fn dht_off_serves_no_kad_alpn() {
    let node = spawn_node(WorkerCfg::Echo).await;
    assert!(node.node.dht().is_none());
    let dialer = loopback_endpoint().await;
    let connected = tokio::time::timeout(CONVERGES, dialer.connect(node.node.addr(), ALPN_KAD))
        .await
        .expect("the dial ends");
    assert!(
        connected.is_err(),
        "a node with the DHT off serves agentnet-kad/0"
    );
    let res = gateway_request(&node.gateway_url(), "GET", "/dht", &[], None).await;
    assert_eq!(res.status, 404, "{:?}", res.body);
}

#[tokio::test]
async fn a_node_with_no_bootstrap_knows_no_one() {
    for mode in [DhtMode::Router, DhtMode::Client] {
        let node = spawn_dht_node(mode, &[], timing()).await;
        let now = status(&node).await;
        assert_eq!(now.mode, mode);
        assert_eq!(now.routing_peers, 0, "{now:?}");
        assert_eq!(now.connections, 0, "{now:?}");
        assert_eq!(now.connections_accepted, 0, "{now:?}");
    }
}

#[tokio::test]
async fn nodes_join_through_the_bootstrap() {
    let r1 = spawn_dht_node(DhtMode::Router, &[], timing()).await;
    let r2 = spawn_dht_node(DhtMode::Router, &[&r1], timing()).await;
    // Each router has the other in its table.
    wait_for_routers(&r1, 1).await;
    wait_for_routers(&r2, 1).await;
    // The client learns of R2 through R1, its only bootstrap.
    let c = spawn_dht_node(DhtMode::Client, &[&r1], timing()).await;
    let joined = wait_for_routers(&c, 2).await;
    assert_eq!(joined.mode, DhtMode::Client);
    let r1_now = status(&r1).await;
    assert!(r1_now.connections_accepted >= 2, "{r1_now:?}");
    assert_eq!(r1_now.routing_peers, 1, "{r1_now:?}");
}

#[tokio::test]
async fn a_router_with_no_bootstrap_learns_the_others() {
    let r1 = spawn_dht_node(DhtMode::Router, &[], timing()).await;
    let r2 = spawn_dht_node(DhtMode::Router, &[&r1], timing()).await;
    let r3 = spawn_dht_node(DhtMode::Router, &[&r1], timing()).await;
    // Only they dialed R1: it learns them from what they declare.
    wait_for_routers(&r1, 2).await;
    wait_for_routers(&r2, 2).await;
    wait_for_routers(&r3, 2).await;
}

/// Waits until `node` has no connection open, after it had one: then it
/// dealt with what came over each, `identify` included, since a
/// connection stays open while a stream of it is.
async fn wait_for_idle(node: &TestNode) -> DhtStatus {
    wait_for(node, "every connection closed", |s| s.connections == 0).await
}

#[tokio::test]
async fn clients_never_enter_a_routing_table() {
    let short = idle_after(Duration::from_secs(1));
    let r1 = spawn_dht_node(DhtMode::Router, &[], short).await;
    let r2 = spawn_dht_node(DhtMode::Router, &[&r1], short).await;
    let r3 = spawn_dht_node(DhtMode::Router, &[&r1], short).await;
    for router in [&r1, &r2, &r3] {
        wait_for_routers(router, 2).await;
    }
    let c1 = spawn_dht_node(DhtMode::Client, &[&r1], short).await;
    let c2 = spawn_dht_node(DhtMode::Client, &[&r1], short).await;
    // Each client reached every router: each has it in its table.
    wait_for_routers(&c1, 3).await;
    wait_for_routers(&c2, 3).await;
    // Once the clients' connections closed, each router dealt with them.
    for router in [&r1, &r2, &r3] {
        let now = wait_for_idle(router).await;
        assert_eq!(now.routing_peers, 2, "only routers: {now:?}");
    }
}

#[tokio::test]
async fn a_client_stays_a_client() {
    let short = idle_after(Duration::from_secs(1));
    let r1 = spawn_dht_node(DhtMode::Router, &[], short).await;
    let c = spawn_dht_node(DhtMode::Client, &[&r1], short).await;
    wait_for(&r1, "the client connected", |s| s.connections_accepted >= 1).await;
    // Once the client's connection closed, R1 dealt with it.
    let now = wait_for_idle(&r1).await;
    assert_eq!(now.routing_peers, 0, "the client is in the table: {now:?}");
    // The client declares an external address, which in libp2p-kad's
    // automatic mode would make it a server.
    let now = status(&c).await;
    assert_eq!(now.mode, DhtMode::Client, "{now:?}");
}

#[tokio::test]
async fn a_newcomer_joins_through_a_saved_contact_when_the_bootstrap_is_down() {
    let r1 = spawn_dht_node(DhtMode::Router, &[], timing()).await;
    let r2 = spawn_dht_node(DhtMode::Router, &[&r1], timing()).await;
    wait_for_routers(&r1, 1).await;
    wait_for_routers(&r2, 1).await;
    let r1_ticket = r1.node.ticket();
    let (r2_id, r2_ticket) = (r2.id(), r2.node.ticket());
    let TestNode { node: r1_node, .. } = r1;
    shut_down(r1_node).await;
    let accepted_before = status(&r2).await.connections_accepted;

    // A newcomer that knows R2 only as a saved contact.
    let dir = with_contacts(&[("r2", r2_id, r2_ticket)]);
    let n = spawn_dht_node_in(
        dir,
        DhtMode::Client,
        &[],
        NodeOptions {
            dht_timing: Some(timing()),
            ..NodeOptions::default()
        },
        |cfg| cfg.dht.bootstrap = vec![r1_ticket],
    )
    .await;
    wait_for(&r2, "the newcomer dialed R2", |s| {
        s.connections_accepted > accepted_before
    })
    .await;
    // R2 is in its table, and R1 with it: libp2p-kad keeps a peer it
    // could not dial, with its last address.
    wait_for_routers(&n, 2).await;
}

/// A fresh data directory whose store already holds `contacts`, by name,
/// agent and ticket.
fn with_contacts(contacts: &[(&str, AgentId, String)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let key = key_in(dir.path());
    let store = Store::open(&dir.path().join(STORE_FILE), key.id()).unwrap();
    for (name, agent_id, ticket) in contacts {
        store
            .contact_put(&Contact {
                name: (*name).to_owned(),
                agent_id: *agent_id,
                ticket: Some(ticket.clone()),
                added_at: now_unix(),
            })
            .unwrap();
    }
    dir
}

#[tokio::test]
async fn saved_contacts_enter_the_table_only_as_routers() {
    let r1 = spawn_dht_node(DhtMode::Router, &[], timing()).await;
    let r2 = spawn_dht_node(DhtMode::Router, &[&r1], timing()).await;
    wait_for_routers(&r1, 1).await;
    wait_for_routers(&r2, 1).await;
    // A DHT client, which serves agentnet-kad/0 but not Kademlia.
    let client = spawn_dht_node(DhtMode::Client, &[&r2], timing()).await;
    wait_for(&r2, "the client connected", |s| s.connections_accepted >= 1).await;
    let r1_ticket = r1.node.ticket();
    let TestNode { node: r1_node, .. } = r1;
    shut_down(r1_node).await;

    // A router whose bootstrap is down, and whose saved contacts, the
    // first 8 by name, are the client and R2.
    let dir = with_contacts(&[
        ("a-client", client.id(), client.node.ticket()),
        ("b-router", r2.id(), r2.node.ticket()),
    ]);
    let n = spawn_dht_node_in(
        dir,
        DhtMode::Router,
        &[],
        NodeOptions {
            dht_timing: Some(idle_after(Duration::from_secs(1))),
            ..NodeOptions::default()
        },
        |cfg| cfg.dht.bootstrap = vec![r1_ticket],
    )
    .await;
    wait_for(&client, "the newcomer dialed the client", |s| {
        s.connections_accepted >= 1
    })
    .await;
    wait_for_routers(&n, 2).await;
    // Once its connections closed, it dealt with both contacts: only R2
    // entered its table, beside R1, kept from its bootstrap ticket.
    let now = wait_for_idle(&n).await;
    assert_eq!(now.routing_peers, 2, "{now:?}");
    // And R2 has R1 and the newcomer, never the client.
    wait_for_routers(&r2, 2).await;
}

/// A DHT client of `libp2p-kad` alone, over its own Iroh endpoint, that
/// takes the connections other nodes open and asks on one of them: what a
/// client does when a node it knows as a router dialed it, and its next
/// lookup goes over that node's connection.
struct Asker {
    swarm: Swarm<kad::Behaviour<MemoryStore>>,
    endpoint: iroh::Endpoint,
    _router: iroh::protocol::Router,
}

impl Asker {
    async fn new() -> Self {
        let endpoint = loopback_endpoint().await;
        let (transport, listener, _refusals) = IrohTransport::new(
            endpoint.clone(),
            KadQuotas::default(),
            std::sync::Arc::new(SystemClock),
            AddressBook::default(),
            Duration::from_secs(60),
        );
        let local = peer_id(&pals::net::agent_id_of(&endpoint.id()));
        let mut config = kad::Config::new(KAD_PROTOCOL);
        config.set_periodic_bootstrap_interval(None);
        let behaviour = kad::Behaviour::with_config(local, MemoryStore::new(local), config);
        let mut swarm = Swarm::new(
            transport.boxed(),
            behaviour,
            local,
            swarm::Config::with_tokio_executor()
                .with_idle_connection_timeout(Duration::from_secs(60)),
        );
        swarm.behaviour_mut().set_mode(Some(Mode::Client));
        let bound = endpoint.bound_sockets()[0];
        let listen = libp2p::Multiaddr::empty()
            .with(libp2p::multiaddr::Protocol::from(bound.ip()))
            .with(libp2p::multiaddr::Protocol::Udp(bound.port()))
            .with(libp2p::multiaddr::Protocol::QuicV1);
        swarm.listen_on(listen).expect("listen on the endpoint");
        let router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(ALPN_KAD, listener)
            .spawn();
        Self {
            swarm,
            endpoint,
            _router: router,
        }
    }

    fn id(&self) -> AgentId {
        pals::net::agent_id_of(&self.endpoint.id())
    }

    /// Its ticket, at its one direct address.
    fn ticket(&self) -> String {
        let bound = self.endpoint.bound_sockets()[0];
        EndpointTicket::new(EndpointAddr::from_parts(
            self.endpoint.id(),
            [TransportAddr::Ip(bound)],
        ))
        .to_string()
    }

    /// The swarm's next event, unless `deadline` comes first.
    async fn next(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<SwarmEvent<kad::Event>, tokio::time::error::Elapsed> {
        tokio::time::timeout_at(deadline, self.swarm.select_next_some()).await
    }

    /// Waits until `node` dials it, and then asks `node`, over that one
    /// connection, for the peers closest to a key, until `node` answered.
    /// It never dials `node`: it knows it at no address it could dial.
    async fn ask_over_the_connection_of(&mut self, node: &TestNode) {
        let dialer = peer_id(&node.id());
        let deadline = tokio::time::Instant::now() + CONVERGES;
        loop {
            match self.next(deadline).await.expect("the node dials") {
                SwarmEvent::ConnectionEstablished {
                    peer_id,
                    endpoint: libp2p::core::ConnectedPoint::Listener { .. },
                    ..
                } if peer_id == dialer => break,
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    panic!("a connection with {peer_id} this side opened")
                }
                _ => {}
            }
        }
        let kad = self.swarm.behaviour_mut();
        kad.add_address(&dialer, located_nowhere(&dialer));
        let id = kad.get_closest_peers(PeerId::random());
        loop {
            let answered = self
                .swarm
                .behaviour_mut()
                .query(&id)
                .is_some_and(|query| query.stats().num_successes() > 0);
            if answered {
                return;
            }
            match self.next(deadline).await.expect("the node answers") {
                SwarmEvent::Behaviour(kad::Event::OutboundQueryProgressed {
                    id: done,
                    stats,
                    step,
                    ..
                }) if done == id && step.last => {
                    assert!(stats.num_successes() > 0, "the node never answered");
                    return;
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } if peer_id == dialer => {
                    panic!("the node's connection closed before it answered")
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn a_client_that_asks_over_a_routers_dial_stays_out_of_its_table() {
    // A client, which the router knows only as a saved contact.
    let mut client = Asker::new().await;
    // A bootstrap router that never answers: the router then dials its
    // saved contacts, the client among them.
    let down = AgentKey::generate().id();
    let down_ticket = EndpointTicket::new(
        EndpointAddr::new(pals::net::endpoint_id_of(&down))
            .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, free_port()))),
    )
    .to_string();
    let dir = with_contacts(&[("client", client.id(), client.ticket())]);
    let router = spawn_dht_node_in(
        dir,
        DhtMode::Router,
        &[],
        NodeOptions {
            dht_timing: Some(timing()),
            ..NodeOptions::default()
        },
        |cfg| cfg.dht.bootstrap = vec![down_ticket],
    )
    .await;
    // The client asks the router on the connection the router dialed:
    // libp2p-kad sees the protocol confirmed on it before it answers.
    client.ask_over_the_connection_of(&router).await;
    // Only the bootstrap router that never answered is in its table:
    // libp2p-kad keeps it with its last address. Never the client.
    let now = status(&router).await;
    assert_eq!(
        now.routing_peers, 1,
        "the client entered the table: {now:?}"
    );
}

#[tokio::test]
async fn a_router_with_two_addresses_is_dialed_once() {
    let r1 = spawn_dht_node(DhtMode::Router, &[], idle_after(Duration::from_secs(1))).await;
    let real = r1.node.addr();
    // A ticket of R1 with an address that never answers before its real one.
    let unreachable = SocketAddr::from(([192, 0, 2, 1], 4433));
    let ticket = EndpointTicket::new(EndpointAddr::from_parts(
        real.id,
        std::iter::once(TransportAddr::Ip(unreachable))
            .chain(real.ip_addrs().copied().map(TransportAddr::Ip)),
    ))
    .to_string();
    let opts = NodeOptions {
        dht_timing: Some(idle_after(Duration::from_secs(1))),
        ..NodeOptions::default()
    };
    let c = spawn_dht_node_with(DhtMode::Client, &[], opts, |cfg| {
        cfg.dht.bootstrap = vec![ticket];
    })
    .await;
    wait_for(&r1, "the client connected", |s| s.connections_accepted >= 1).await;
    // Once the client's connections closed as idle, every dial of its
    // entry is over.
    wait_for(&c, "the client's connections closed", |s| {
        s.connections == 0
    })
    .await;
    let now = status(&r1).await;
    assert_eq!(now.connections_accepted, 1, "{now:?}");
}

#[tokio::test]
async fn connections_beyond_the_per_peer_limit_are_refused() {
    let r1 = spawn_dht_node(DhtMode::Router, &[], timing()).await;
    let dialer = loopback_endpoint().await;
    let mut connections = Vec::new();
    for _ in 0..3 {
        let connection = tokio::time::timeout(CONVERGES, dialer.connect(r1.node.addr(), ALPN_KAD))
            .await
            .expect("the dial ends")
            .expect("connect to the router");
        connections.push(connection);
    }
    let closings: Vec<_> = connections
        .iter()
        .map(|connection| Box::pin(connection.closed()))
        .collect();
    let (reason, closed, _) =
        tokio::time::timeout(CONVERGES, libp2p::futures::future::select_all(closings))
            .await
            .expect("the router closes one of the three");
    // libp2p closes it, with code 0, as it does any connection it drops.
    assert!(
        matches!(
            &reason,
            iroh::endpoint::ConnectionError::ApplicationClosed(close)
                if close.error_code == VarInt::from_u32(0)
        ),
        "{reason:?}"
    );
    for (i, connection) in connections.iter().enumerate() {
        if i != closed {
            assert!(
                connection.close_reason().is_none(),
                "connection {i}: {:?}",
                connection.close_reason()
            );
        }
    }
    let now = status(&r1).await;
    assert_eq!(now.connections_accepted, 2, "{now:?}");
    assert_eq!(now.connections, 2, "{now:?}");
}

#[tokio::test]
async fn idle_connections_close_after_the_idle_timeout() {
    let short = idle_after(Duration::from_secs(1));
    let r1 = spawn_dht_node(DhtMode::Router, &[], short).await;
    let c = spawn_dht_node(DhtMode::Client, &[&r1], short).await;
    wait_for(&r1, "the client connected", |s| s.connections_accepted >= 1).await;
    // Nothing keeps a connection open once its streams are done.
    wait_for(&c, "the client's connections closed", |s| {
        s.connections == 0
    })
    .await;
    wait_for(&r1, "the router's connections closed", |s| {
        s.connections == 0
    })
    .await;
    assert_eq!(status(&c).await.routing_peers, 1);
}

#[tokio::test]
async fn an_idle_inbound_stream_is_reset_and_frees_its_slot() {
    let opts = NodeOptions {
        dht_timing: Some(idle_after(Duration::from_secs(2))),
        kad_quotas: KadQuotas {
            peer_streams: 1,
            ..KadQuotas::default()
        },
        ..NodeOptions::default()
    };
    let r1 = spawn_dht_node_with(DhtMode::Router, &[], opts, |_| {}).await;
    let dialer = loopback_endpoint().await;
    let connection = tokio::time::timeout(CONVERGES, dialer.connect(r1.node.addr(), ALPN_KAD))
        .await
        .expect("the dial ends")
        .expect("connect to the router");

    // A stream that negotiates Kademlia and then says nothing more.
    let (_idle_send, mut idle) = open_kad_stream(&connection).await;
    assert_eq!(negotiated(&mut idle).await.unwrap(), NEGOTIATION);
    // It holds the peer's one stream: the next is refused.
    let (_send, mut refused) = open_kad_stream(&connection).await;
    assert_eq!(
        negotiated(&mut refused).await.unwrap_err(),
        ReadError::Reset(VarInt::from_u32(REFUSED))
    );
    // Without a read or a write for the idle timeout, the router resets it.
    assert_eq!(
        negotiated(&mut idle).await.unwrap_err(),
        ReadError::Reset(VarInt::from_u32(REFUSED))
    );
    let now = status(&r1).await;
    assert_eq!(now.refused.idle_streams, 1, "{now:?}");
    assert_eq!(now.refused.streams, 1, "{now:?}");
    // Its slot is free again.
    let (_send, mut next) = open_kad_stream(&connection).await;
    assert_eq!(negotiated(&mut next).await.unwrap(), NEGOTIATION);
}

#[tokio::test]
async fn a_routers_ticket_names_its_external_addrs() {
    for ip in [Ipv4Addr::LOCALHOST, Ipv4Addr::UNSPECIFIED] {
        let port = free_port();
        let listen = SocketAddr::from((ip, port));
        let external = vec![
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            SocketAddr::from(([192, 0, 2, 7], 4433)),
        ];
        let opts = NodeOptions {
            dht_timing: Some(timing()),
            ..NodeOptions::default()
        };
        let declared = external.clone();
        let router = spawn_dht_node_with(DhtMode::Router, &[], opts, |cfg| {
            cfg.net.listen = Some(listen);
            cfg.dht.external_addrs = declared;
        })
        .await;
        let (code, out, err) = pals(cli_home(&router), &["id", "--ticket"]).await;
        assert_eq!(code, 0, "{err}");
        let ticket: EndpointTicket = out.trim().parse().unwrap();
        let addrs: Vec<SocketAddr> = ticket.endpoint_addr().ip_addrs().copied().collect();
        for addr in &external {
            assert!(
                addrs.contains(addr),
                "listen {listen}: {addr} not in {addrs:?}"
            );
        }
        assert!(
            addrs.iter().all(|addr| !addr.ip().is_unspecified()),
            "listen {listen}: {addrs:?}"
        );
        // The node's own address says the same.
        let node_addrs: Vec<SocketAddr> = router.node.addr().ip_addrs().copied().collect();
        for addr in &external {
            assert!(node_addrs.contains(addr), "listen {listen}: {node_addrs:?}");
        }
        assert!(
            node_addrs.iter().all(|addr| !addr.ip().is_unspecified()),
            "listen {listen}: {node_addrs:?}"
        );
    }
}

#[tokio::test]
async fn status_through_the_gateway_and_the_cli() {
    let r1 = spawn_dht_node(DhtMode::Router, &[], timing()).await;
    let r2 = spawn_dht_node(DhtMode::Router, &[&r1], timing()).await;
    wait_for_routers(&r1, 1).await;
    wait_for_routers(&r2, 1).await;
    let home = cli_home(&r1);
    let through_gateway = || async {
        let res = gateway_request(&r1.gateway_url(), "GET", "/dht", &[], None).await;
        assert_eq!(res.status, 200, "{:?}", res.body);
        res.json
    };
    let deadline = Instant::now() + CONVERGES;
    loop {
        let before = through_gateway().await;
        let (code, out, err) = pals(home, &["dht", "status", "--json"]).await;
        assert_eq!(code, 0, "{err}");
        let after = through_gateway().await;
        // The status did not change meanwhile: both name the same one.
        if before == after {
            let printed: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(printed, before);
            assert_eq!(before["mode"], "router");
            assert_eq!(before["routing_peers"], 1);
            break;
        }
        assert!(Instant::now() < deadline, "the status never held still");
    }
    let (code, out, err) = pals(home, &["dht", "status"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("mode: router"), "{out}");
    assert!(out.contains("routing peers: 1"), "{out}");
}

#[tokio::test]
async fn bad_dht_sections_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CONFIG_FILE);
    let other = AgentKey::generate().id();
    let ticket_of = |addr: EndpointAddr| EndpointTicket::new(addr).to_string();
    let addressed = ticket_of(
        EndpointAddr::new(pals::net::endpoint_id_of(&other))
            .with_ip_addr(SocketAddr::from(([127, 0, 0, 1], 4433))),
    );
    let unaddressed = ticket_of(EndpointAddr::new(pals::net::endpoint_id_of(&other)));
    let relayed = ticket_of(
        EndpointAddr::new(pals::net::endpoint_id_of(&other))
            .with_relay_url("https://relay.example.org".parse().unwrap()),
    );
    for (section, word) in [
        ("bootstrap = [\"not a ticket\"]".to_owned(), "dht.bootstrap"),
        (format!("bootstrap = [\"{unaddressed}\"]"), "dht.bootstrap"),
        (format!("bootstrap = [\"{relayed}\"]"), "dht.bootstrap"),
        (
            "record_ttl_s = 3600\nrepublish_s = 1801".to_owned(),
            "dht.republish_s",
        ),
        ("republish_s = 19".to_owned(), "dht.republish_s"),
        ("record_ttl_s = 59".to_owned(), "dht.record_ttl_s"),
        ("record_ttl_s = 86401".to_owned(), "dht.record_ttl_s"),
        (
            "external_addrs = [\"192.0.2.7:4433\"]".to_owned(),
            "dht.external_addrs",
        ),
        (
            "mode = \"router\"\nexternal_addrs = [\"0.0.0.0:4433\"]".to_owned(),
            "dht.external_addrs",
        ),
        ("mode = \"server\"".to_owned(), "unknown variant"),
        ("dial = true".to_owned(), "unknown field `dial`"),
    ] {
        let text = format!("[dht]\nenabled = true\n{section}\n");
        std::fs::write(&path, &text).unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(format!("{err:#}").contains(word), "{text}: {err:#}");
    }
    // What a router, with the relay too, and a client may say.
    for section in [
        format!(
            "mode = \"router\"\nexternal_addrs = [\"192.0.2.7:4433\"]\nbootstrap = [\"{addressed}\"]"
        ),
        "mode = \"client\"\nrecord_ttl_s = 60\nrepublish_s = 30".to_owned(),
        "mode = \"router\"\nrecord_ttl_s = 86400\nrepublish_s = 43200\nannounce = true".to_owned(),
    ] {
        for relay in [true, false] {
            let text = format!("[net]\nrelay = {relay}\n\n[dht]\nenabled = true\n{section}\n");
            std::fs::write(&path, &text).unwrap();
            Config::load(&path).unwrap_or_else(|err| panic!("{text}: {err:#}"));
        }
    }
    // Off by default, a client, announcing nothing, with no bootstrap.
    std::fs::write(&path, "").unwrap();
    let cfg = Config::load(&path).unwrap();
    assert_eq!(
        cfg.dht,
        DhtCfg {
            enabled: false,
            mode: DhtMode::Client,
            bootstrap: Vec::new(),
            external_addrs: Vec::new(),
            announce: false,
            record_ttl_s: 3600,
            republish_s: 1200,
        }
    );

    // The node's own ticket: refused when the node starts, which knows its
    // key.
    let dir = tempfile::tempdir().unwrap();
    let key = key_in(dir.path());
    let own = ticket_of(
        EndpointAddr::new(pals::net::endpoint_id_of(&key.id()))
            .with_ip_addr(SocketAddr::from(([127, 0, 0, 1], 4433))),
    );
    let mut cfg = common::test_config(dir.path(), WorkerCfg::Echo);
    cfg.dht = DhtCfg {
        enabled: true,
        bootstrap: vec![own],
        ..DhtCfg::default()
    };
    let err = match Node::spawn(cfg).await {
        Ok(_) => panic!("a node started with its own ticket as bootstrap"),
        Err(err) => err,
    };
    assert!(format!("{err:#}").contains("dht.bootstrap"), "{err:#}");
    assert!(format!("{err:#}").contains("own"), "{err:#}");
}

/// A swarm of `libp2p-kad` alone over its own Iroh endpoint, as a client
/// with `key` would publish: the test's stand-in for the publication of
/// task 12.
struct Publisher {
    swarm: Swarm<kad::Behaviour<MemoryStore>>,
    _endpoint: iroh::Endpoint,
}

impl Publisher {
    async fn new(key: &AgentKey, router: &TestNode) -> Self {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(iroh::RelayMode::Disabled)
            .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let (transport, _listener, _refusals) = IrohTransport::new(
            endpoint.clone(),
            KadQuotas::default(),
            std::sync::Arc::new(SystemClock),
            AddressBook::default(),
            Duration::from_secs(10),
        );
        let local = peer_id(&key.id());
        let mut config = kad::Config::new(KAD_PROTOCOL);
        config.set_publication_interval(None);
        config.set_provider_publication_interval(None);
        let behaviour = kad::Behaviour::with_config(local, MemoryStore::new(local), config);
        let mut swarm = Swarm::new(
            transport.boxed(),
            behaviour,
            local,
            swarm::Config::with_tokio_executor(),
        );
        swarm.behaviour_mut().set_mode(Some(Mode::Client));
        swarm.add_external_address(located_nowhere(&local));
        let router_peer = peer_id(&router.id());
        for addr in router.node.addr().ip_addrs() {
            swarm
                .behaviour_mut()
                .add_address(&router_peer, multiaddr_of(&router_peer, *addr));
        }
        Self {
            swarm,
            _endpoint: endpoint,
        }
    }

    /// Drives the swarm until query `id` made its last step.
    async fn finish(&mut self, id: kad::QueryId) -> kad::QueryResult {
        let deadline = tokio::time::Instant::now() + CONVERGES;
        loop {
            let event = tokio::time::timeout_at(deadline, self.swarm.select_next_some())
                .await
                .expect("the query ends");
            if let SwarmEvent::Behaviour(kad::Event::OutboundQueryProgressed {
                id: done,
                result,
                step,
                ..
            }) = event
                && done == id
                && step.last
            {
                return result;
            }
        }
    }

    /// Runs the swarm for `pause`: `libp2p-kad` reports a provider record
    /// sent once the connection has it, which writes it only while the
    /// swarm runs.
    async fn run_for(&mut self, pause: Duration) {
        let _ = tokio::time::timeout(pause, async {
            loop {
                self.swarm.select_next_some().await;
            }
        })
        .await;
    }

    /// Puts `value` under `key` at the router.
    async fn put(&mut self, key: libp2p::kad::RecordKey, value: Vec<u8>) {
        let id = self
            .swarm
            .behaviour_mut()
            .put_record(Record::new(key, value), Quorum::One)
            .unwrap();
        let result = self.finish(id).await;
        assert!(
            matches!(result, kad::QueryResult::PutRecord(Ok(_))),
            "{result:?}"
        );
    }

    /// Provides `capability` at the router.
    async fn provide(&mut self, capability: &str) {
        let id = self
            .swarm
            .behaviour_mut()
            .start_providing(provider_key(capability).unwrap())
            .unwrap();
        let result = self.finish(id).await;
        assert!(
            matches!(result, kad::QueryResult::StartProviding(Ok(_))),
            "{result:?}"
        );
    }
}

/// `key`'s announce with `seq`, listing `capabilities`, valid now.
fn announce(key: &AgentKey, seq: u64, capabilities: &[&str]) -> Vec<u8> {
    let issued_at = now_unix() - 60;
    let doc = AnnounceRecord {
        schema: ANNOUNCE_SCHEMA.to_owned(),
        agent_id: key.id(),
        seq,
        agent_card_hash: format!("blake3:{}", "ab".repeat(32))
            .parse::<CardHash>()
            .unwrap(),
        capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
        discovery_alpn: "agentnet-discovery/0".to_owned(),
        issued_at: format_timestamp(issued_at).unwrap(),
        expires_at: format_timestamp(issued_at + RECORD_TTL_S).unwrap(),
    };
    Signed::issue(key, doc).unwrap().to_json()
}

/// The agents whose provider records of `capability` `router` holds.
async fn holders(router: &TestNode, capability: &str) -> Vec<AgentId> {
    router
        .node
        .dht()
        .unwrap()
        .held_providers(capability)
        .await
        .unwrap()
        .into_iter()
        .map(|(agent, _)| agent)
        .collect()
}

/// Runs `publishers` until `router` holds, for each capability of `want`,
/// the provider records of exactly those agents.
async fn wait_for_holders(
    router: &TestNode,
    publishers: &mut [&mut Publisher],
    want: &[(&str, &[AgentId])],
) {
    let deadline = Instant::now() + CONVERGES;
    loop {
        let mut held = true;
        for (capability, agents) in want {
            let mut holding = holders(router, capability).await;
            holding.sort();
            let mut agents = agents.to_vec();
            agents.sort();
            held &= holding == agents;
        }
        if held {
            return;
        }
        if Instant::now() >= deadline {
            panic!("the router never held {want:?}: {:?}", status(router).await);
        }
        for publisher in publishers.iter_mut() {
            publisher.run_for(Duration::from_millis(25)).await;
        }
    }
}

#[tokio::test]
async fn a_newer_announce_drops_the_providers_it_no_longer_lists() {
    let r1 = spawn_dht_node(DhtMode::Router, &[], timing()).await;
    let (a_key, b_key) = (AgentKey::generate(), AgentKey::generate());
    let (a, b) = (a_key.id(), b_key.id());
    let mut pa = Publisher::new(&a_key, &r1).await;
    let mut pb = Publisher::new(&b_key, &r1).await;
    let (ocr, review) = ("vision.ocr", "code.rust.review");
    pa.put(announce_key(&a), announce(&a_key, 1, &[ocr, review]))
        .await;
    pa.provide(ocr).await;
    pa.provide(review).await;
    pb.put(announce_key(&b), announce(&b_key, 1, &[review]))
        .await;
    pb.provide(review).await;
    wait_for_holders(
        &r1,
        &mut [&mut pa, &mut pb],
        &[(ocr, &[a]), (review, &[a, b])],
    )
    .await;
    let held = status(&r1).await;
    assert_eq!(
        held.stored_by_agent,
        BTreeMap::from([(a, 3), (b, 2)]),
        "{held:?}"
    );

    // A's new announce lists only one of the two: A's other provider
    // record goes, and B's under the same key stays.
    pa.put(announce_key(&a), announce(&a_key, 2, &[ocr])).await;
    wait_for_holders(&r1, &mut [&mut pa, &mut pb], &[(ocr, &[a]), (review, &[b])]).await;
    let held = status(&r1).await;
    assert_eq!(
        held.stored_by_agent,
        BTreeMap::from([(a, 2), (b, 2)]),
        "{held:?}"
    );
    assert_eq!(held.provider_records_stored, 2, "{held:?}");
    assert_eq!(held.records_stored, 2, "{held:?}");
    assert!(held.rejected.values().all(|n| *n == 0), "{held:?}");

    // An older announce of A's, and one under A's key that B signed, each
    // listing only the other capability: the first is kept out as stale,
    // the second refused, and neither drops anything. A put ends once the
    // router answered, which it does after it dealt with the record.
    pa.put(announce_key(&a), announce(&a_key, 1, &[review]))
        .await;
    pb.put(announce_key(&a), announce(&b_key, 3, &[review]))
        .await;
    let held = wait_for(&r1, "the forged announce refused", |s| {
        s.rejected["wrong_key"] == 1
    })
    .await;
    assert_eq!(holders(&r1, ocr).await, vec![a]);
    assert_eq!(holders(&r1, review).await, vec![b]);
    assert_eq!(
        held.stored_by_agent,
        BTreeMap::from([(a, 2), (b, 2)]),
        "{held:?}"
    );
    assert_eq!(held.rejected.values().sum::<u64>(), 1, "{held:?}");
}
