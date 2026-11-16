//! The libp2p transport over the node's Iroh endpoint, on its own: real
//! endpoints on loopback, each with an agent's key, dialing and accepting
//! through `IrohTransport` as a libp2p swarm would, with the limits of
//! `agentnet-kad/0` (its accept queue and its stream quotas), the address
//! forms frozen by `kad-v0.json`, and `[net] listen`.
//!
//! These tests are the gate of any change of `libp2p` or `iroh` versions
//! (H3-D1, condition 3).

mod common;

use std::future::poll_fn;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use iroh::endpoint::{ConnectionError, PortmapperConfig, ReadError, VarInt, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};
use libp2p::core::muxing::{StreamMuxerBox, StreamMuxerExt, SubstreamBox};
use libp2p::core::transport::{DialOpts, ListenerId, PortUse, TransportEvent};
use libp2p::core::{Endpoint as Role, Transport};
use libp2p::futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::{Multiaddr, PeerId};
use pals::config::{Config, WorkerCfg};
use pals::dht::IDLE_TIMEOUT;
use pals::dht::keys::{agent_of, peer_id};
use pals::dht::transport::{
    self, ALPN_KAD, AddressBook, IrohListener, IrohTransport, KadQuotas, REFUSED, Refusals,
    dial_target, located_nowhere, multiaddr_of,
};
use pals_core::clock::{Clock, ManualClock, SystemClock};
use pals_core::identity::AgentKey;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

/// How long a test waits for what must come at once: a hang fails it.
const PROMPTLY: Duration = Duration::from_secs(10);

/// One node: its transport, listening, and the router that hands it the
/// connections of [`ALPN_KAD`].
struct Node {
    transport: IrohTransport,
    refusals: Refusals,
    /// The socket its endpoint is bound to, on 127.0.0.1.
    socket: SocketAddr,
    peer: PeerId,
    _router: Router,
}

fn agent(seed: u8) -> AgentKey {
    AgentKey::from_bytes(&[seed; 32])
}

/// An endpoint as the daemon binds it with `relay = false`: the agent's
/// key, 127.0.0.1 on a free port, no relay, no port mapping.
async fn endpoint(agent: &AgentKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&agent.to_bytes()))
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .clear_ip_transports()
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// The socket `endpoint` is bound to.
fn socket_of(endpoint: &Endpoint) -> SocketAddr {
    endpoint.bound_sockets()[0]
}

/// `/ip4/<ip>/udp/<port>/quic-v1`: the router form without its `/p2p/`.
fn quic_addr(peer: &PeerId, socket: SocketAddr) -> Multiaddr {
    let mut addr = multiaddr_of(peer, socket);
    addr.pop();
    addr
}

async fn node(agent: &AgentKey) -> Node {
    node_with(
        agent,
        KadQuotas::default(),
        Arc::new(SystemClock),
        AddressBook::default(),
    )
    .await
}

async fn node_with(
    agent: &AgentKey,
    quotas: KadQuotas,
    clock: Arc<dyn Clock>,
    book: AddressBook,
) -> Node {
    let endpoint = endpoint(agent).await;
    let socket = socket_of(&endpoint);
    let peer = peer_id(&agent.id());
    let (mut transport, listener, refusals) =
        IrohTransport::new(endpoint.clone(), quotas, clock, book, IDLE_TIMEOUT);
    let router = Router::builder(endpoint).accept(ALPN_KAD, listener).spawn();
    // Port 0 names whatever port the endpoint is bound to.
    let wanted = quic_addr(&peer, SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
    transport.listen_on(ListenerId::next(), wanted).unwrap();
    match next_event(&mut transport).await {
        TransportEvent::NewAddress { listen_addr, .. } => {
            assert_eq!(listen_addr, quic_addr(&peer, socket));
        }
        other => panic!("not a new address: {other:?}"),
    }
    Node {
        transport,
        refusals,
        socket,
        peer,
        _router: router,
    }
}

/// The transport's next event, as the swarm polls it.
async fn next_event(
    transport: &mut IrohTransport,
) -> TransportEvent<<IrohTransport as Transport>::ListenerUpgrade, transport::Error> {
    poll_fn(|cx| Pin::new(&mut *transport).poll(cx)).await
}

fn dialer() -> DialOpts {
    DialOpts {
        role: Role::Dialer,
        port_use: PortUse::Reuse,
    }
}

/// `from` dials `to` at its own address, and `to` accepts: what each side
/// gets, the `PeerId` it sees and the connection.
async fn connect(
    from: &mut Node,
    to: &mut Node,
) -> ((PeerId, StreamMuxerBox), (PeerId, StreamMuxerBox)) {
    let addr = multiaddr_of(&to.peer, to.socket);
    connect_at(from, to, addr).await
}

/// [`connect`], dialing `addr`.
async fn connect_at(
    from: &mut Node,
    to: &mut Node,
    addr: Multiaddr,
) -> ((PeerId, StreamMuxerBox), (PeerId, StreamMuxerBox)) {
    let dial = from.transport.dial(addr, dialer()).unwrap();
    let accept = async {
        loop {
            if let TransportEvent::Incoming { upgrade, .. } = next_event(&mut to.transport).await {
                break upgrade.await;
            }
        }
    };
    timeout(PROMPTLY, async { tokio::try_join!(dial, accept) })
        .await
        .expect("connecting hangs")
        .expect("the dial connects")
}

/// `opener` opens a substream and sends `message`; `acceptor` takes the
/// substream, reads it and answers with it backwards.
async fn round_trip(opener: &mut StreamMuxerBox, acceptor: &mut StreamMuxerBox, message: &[u8]) {
    let mut outbound = open(opener, message).await;
    let mut inbound = poll_fn(|cx| acceptor.poll_inbound_unpin(cx)).await.unwrap();
    answer(&mut outbound, &mut inbound, message).await;
}

/// A substream `muxer` opens, with `message` sent on it: the other side
/// learns of a QUIC stream only once something is sent on it.
async fn open(muxer: &mut StreamMuxerBox, message: &[u8]) -> SubstreamBox {
    let mut outbound = timeout(PROMPTLY, poll_fn(|cx| muxer.poll_outbound_unpin(cx)))
        .await
        .expect("opening hangs")
        .unwrap();
    outbound.write_all(message).await.unwrap();
    outbound.flush().await.unwrap();
    outbound
}

/// `inbound` reads `message`, which `outbound` sent, and answers it
/// backwards; `outbound` reads the answer to its end.
async fn answer(outbound: &mut SubstreamBox, inbound: &mut SubstreamBox, message: &[u8]) {
    let mut received = vec![0; message.len()];
    inbound.read_exact(&mut received).await.unwrap();
    assert_eq!(received, message);
    let reply: Vec<u8> = message.iter().rev().copied().collect();
    inbound.write_all(&reply).await.unwrap();
    inbound.close().await.unwrap();

    outbound.close().await.unwrap();
    let mut back = Vec::new();
    timeout(PROMPTLY, outbound.read_to_end(&mut back))
        .await
        .expect("reading the answer hangs")
        .unwrap();
    assert_eq!(back, reply);
}

/// Accepts every substream of `muxer` in the background, as libp2p would,
/// and hands each one over.
fn accept_all(mut muxer: StreamMuxerBox) -> mpsc::UnboundedReceiver<SubstreamBox> {
    let (accepted, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(substream) = poll_fn(|cx| muxer.poll_inbound_unpin(cx)).await {
            if accepted.send(substream).is_err() {
                break;
            }
        }
    });
    receiver
}

/// The next substream [`accept_all`] took.
async fn accepted(substreams: &mut mpsc::UnboundedReceiver<SubstreamBox>) -> SubstreamBox {
    timeout(PROMPTLY, substreams.recv())
        .await
        .expect("no substream accepted")
        .expect("the connection is gone")
}

/// `outbound` was reset by the other side with [`REFUSED`], unread.
async fn assert_refused(outbound: &mut SubstreamBox) {
    let mut rest = Vec::new();
    let error = timeout(PROMPTLY, outbound.read_to_end(&mut rest))
        .await
        .expect("reading a refused substream hangs")
        .expect_err("a refused substream is reset");
    let read = error.get_ref().and_then(|e| e.downcast_ref::<ReadError>());
    assert_eq!(
        read,
        Some(&ReadError::Reset(VarInt::from_u32(REFUSED))),
        "{error:?}"
    );
    assert!(rest.is_empty(), "nothing was answered: {rest:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_open_a_substream_both_ways() {
    let (mut a, mut b) = (node(&agent(1)).await, node(&agent(2)).await);
    let ((_, mut at_a), (_, mut at_b)) = connect(&mut a, &mut b).await;
    round_trip(&mut at_a, &mut at_b, b"from the dialer").await;
    round_trip(&mut at_b, &mut at_a, b"from the listener").await;
    // And again: each substream is a QUIC stream of its own.
    round_trip(&mut at_a, &mut at_b, b"once more").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn peer_id_is_the_agent_key() {
    let (alice, bob) = (agent(1), agent(2));
    let (mut a, mut b) = (node(&alice).await, node(&bob).await);
    let ((bob_seen, _), (alice_seen, _)) = connect(&mut a, &mut b).await;
    assert_eq!(bob_seen, peer_id(&bob.id()));
    assert_eq!(alice_seen, peer_id(&alice.id()));
    // Each one's PeerId names its agent back.
    assert_eq!(agent_of(&bob_seen), Some(bob.id()));
    assert_eq!(agent_of(&alice_seen), Some(alice.id()));
}

#[tokio::test(flavor = "multi_thread")]
async fn dial_with_a_peer_id_not_matching_the_endpoint_fails() {
    let (mut a, b) = (node(&agent(1)).await, node(&agent(2)).await);
    // B's address with C's PeerId: Iroh's TLS finds B's key, not C's.
    let wrong = multiaddr_of(&peer_id(&agent(3).id()), b.socket);
    let error = timeout(PROMPTLY, a.transport.dial(wrong, dialer()).unwrap())
        .await
        .expect("dialing hangs")
        .unwrap_err();
    assert!(
        matches!(error, transport::Error::Connect(_)),
        "refused by Iroh's handshake, not by a timeout: {error:?}"
    );
    // An address with no PeerId names no Iroh endpoint.
    assert!(
        a.transport
            .dial(quic_addr(&b.peer, b.socket), dialer())
            .is_err()
    );
    // Nor does one with a PeerId that is no agent's.
    assert!(
        a.transport
            .dial(multiaddr_of(&PeerId::random(), b.socket), dialer())
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_connection_ends_on_both_sides() {
    let (mut a, mut b) = (node(&agent(1)).await, node(&agent(2)).await);
    let ((_, mut at_a), (_, mut at_b)) = connect(&mut a, &mut b).await;
    round_trip(&mut at_a, &mut at_b, b"before closing").await;
    // A substream A keeps open across the close.
    let mut open = open(&mut at_a, b"still here").await;

    // B closes, as libp2p does an idle connection.
    timeout(PROMPTLY, poll_fn(|cx| at_b.poll_close_unpin(cx)))
        .await
        .expect("closing hangs")
        .expect("a local close is clean");

    // A learns it on every side of the connection, without hanging.
    let inbound = timeout(PROMPTLY, poll_fn(|cx| at_a.poll_inbound_unpin(cx)))
        .await
        .expect("accepting hangs");
    assert!(
        inbound.is_err(),
        "no substream comes from a closed connection"
    );
    let outbound = timeout(PROMPTLY, poll_fn(|cx| at_a.poll_outbound_unpin(cx)))
        .await
        .expect("opening hangs");
    assert!(
        outbound.is_err(),
        "no substream opens on a closed connection"
    );
    let mut rest = Vec::new();
    let read = timeout(PROMPTLY, open.read_to_end(&mut rest))
        .await
        .expect("reading hangs");
    // It ends with the connection lost: B never accepted that substream, so
    // it never wrote on it, let alone finished it.
    assert!(read.is_err(), "{read:?} {rest:?}");
    // Closing A's side completes too. Whether it reports B's close or its
    // own (noq reports its own) does not matter: the connection is gone.
    let _closed = timeout(PROMPTLY, poll_fn(|cx| at_a.poll_close_unpin(cx)))
        .await
        .expect("closing hangs");
}

#[tokio::test(flavor = "multi_thread")]
async fn listen_on_wakes_a_waiting_swarm() {
    // The swarm polls the transport, which has nothing to say yet, and only
    // then asks it to listen: the new address must reach the swarm without
    // any other event to wake it.
    let key = agent(1);
    let endpoint = endpoint(&key).await;
    let socket = socket_of(&endpoint);
    let (transport, listener, _refusals) = IrohTransport::new(
        endpoint.clone(),
        KadQuotas::default(),
        Arc::new(SystemClock),
        AddressBook::default(),
        IDLE_TIMEOUT,
    );
    let _router = Router::builder(endpoint).accept(ALPN_KAD, listener).spawn();
    let transport = Arc::new(Mutex::new(transport));
    let (polled, waiting) = oneshot::channel();
    let swarm = tokio::spawn({
        let transport = Arc::clone(&transport);
        let mut polled = Some(polled);
        async move {
            let event = poll_fn(|cx| {
                let event = Pin::new(&mut *transport.lock().unwrap()).poll(cx);
                if let Some(polled) = polled.take() {
                    let _ = polled.send(());
                }
                event
            })
            .await;
            matches!(event, TransportEvent::NewAddress { .. })
        }
    });
    waiting.await.unwrap();
    transport
        .lock()
        .unwrap()
        .listen_on(ListenerId::next(), quic_addr(&peer_id(&key.id()), socket))
        .unwrap();
    let new_address = timeout(PROMPTLY, swarm)
        .await
        .expect("the waiting swarm was never woken")
        .unwrap();
    assert!(new_address);
}

fn vector() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../pals-core/tests/vectors/kad-v0.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn addresses_follow_the_frozen_forms() {
    let vector = vector();
    assert_eq!(ALPN_KAD, vector["alpn"].as_str().unwrap().as_bytes());
    let peer = PeerId::from_str(vector["peer_id"].as_str().unwrap()).unwrap();
    let at: SocketAddr = "192.0.2.10:4433".parse().unwrap();

    let router = multiaddr_of(&peer, at);
    assert_eq!(router.to_string(), vector["addresses"]["router"]);
    assert_eq!(hex(&router.to_vec()), vector["addresses_hex"]["router"]);
    assert_eq!(dial_target(&router), Some((peer, at)));
    // IPv6 has the same form.
    let at6: SocketAddr = "[2001:db8::10]:4433".parse().unwrap();
    let router6 = multiaddr_of(&peer, at6);
    assert_eq!(
        router6.to_string(),
        format!("/ip6/2001:db8::10/udp/4433/quic-v1/p2p/{peer}")
    );
    assert_eq!(dial_target(&router6), Some((peer, at6)));

    let client = located_nowhere(&peer);
    assert_eq!(client.to_string(), vector["addresses"]["client"]);
    assert_eq!(hex(&client.to_vec()), vector["addresses_hex"]["client"]);
    // Only the router form is dialed.
    assert_eq!(dial_target(&client), None);
    for other in [
        format!("/ip4/192.0.2.10/tcp/4433/p2p/{peer}"),
        format!("/ip4/192.0.2.10/udp/4433/quic/p2p/{peer}"),
        format!("/ip4/192.0.2.10/udp/4433/quic-v1/p2p/{peer}/p2p/{peer}"),
        "/ip4/192.0.2.10/udp/4433/quic-v1".to_owned(),
        format!("/dns4/example.org/udp/4433/quic-v1/p2p/{peer}"),
        // A PeerId that is no agent's names no endpoint.
        format!("/ip4/192.0.2.10/udp/4433/quic-v1/p2p/{}", PeerId::random()),
    ] {
        let addr = Multiaddr::from_str(&other).unwrap();
        assert_eq!(dial_target(&addr), None, "{other}");
    }
}

/// [`IrohListener`], telling the test each time its `accept` returns.
#[derive(Debug, Clone)]
struct Watched {
    listener: IrohListener,
    returned: mpsc::UnboundedSender<()>,
}

impl ProtocolHandler for Watched {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let accepted = self.listener.accept(connection).await;
        let _ = self.returned.send(());
        accepted
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_accept_queue_refuses_instead_of_waiting() {
    let b = endpoint(&agent(2)).await;
    let quotas = KadQuotas {
        accept_queue: 1,
        ..KadQuotas::default()
    };
    // B's transport is never polled: nothing takes from its queue.
    let (_transport, listener, refusals) = IrohTransport::new(
        b.clone(),
        quotas,
        Arc::new(SystemClock),
        AddressBook::default(),
        IDLE_TIMEOUT,
    );
    let (returned, mut accepts) = mpsc::unbounded_channel();
    let _router = Router::builder(b.clone())
        .accept(ALPN_KAD, Watched { listener, returned })
        .spawn();
    let b_addr = EndpointAddr::from_parts(b.id(), [TransportAddr::Ip(socket_of(&b))]);
    let a = endpoint(&agent(1)).await;

    let first = a.connect(b_addr.clone(), ALPN_KAD).await.unwrap();
    timeout(PROMPTLY, accepts.recv())
        .await
        .expect("the first connection was never queued");

    let second = a.connect(b_addr, ALPN_KAD).await.unwrap();
    timeout(PROMPTLY, accepts.recv())
        .await
        .expect("the listener waits for room in its queue");
    let closed = timeout(PROMPTLY, second.closed())
        .await
        .expect("the second connection is left open");
    assert!(
        matches!(
            &closed,
            ConnectionError::ApplicationClosed(close)
                if close.error_code == VarInt::from_u32(REFUSED)
        ),
        "{closed:?}"
    );
    assert_eq!(refusals.counts().connections, 1);
    assert_eq!(refusals.counts().streams, 0);
    // The first one waits in the queue, still open.
    assert!(first.close_reason().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dial_carries_every_known_address() {
    let book = AddressBook::default();
    let mut a = node_with(
        &agent(1),
        KadQuotas::default(),
        Arc::new(SystemClock),
        book.clone(),
    )
    .await;
    let mut b = node(&agent(2)).await;
    // An address where nothing answers, and B's real one.
    let nowhere: SocketAddr = "192.0.2.1:9".parse().unwrap();
    book.learn(b.peer, nowhere);
    book.learn(b.peer, b.socket);
    assert_eq!(book.of(&b.peer), vec![nowhere, b.socket]);

    // Dialing the first connects: the dial carries the second as well.
    let dialed = multiaddr_of(&b.peer, nowhere);
    let ((seen, mut at_a), (_, mut at_b)) = connect_at(&mut a, &mut b, dialed).await;
    assert_eq!(seen, b.peer);
    round_trip(&mut at_a, &mut at_b, b"one connection").await;
    // And B counts a single incoming connection.
    let more = poll_fn(|cx| Poll::Ready(Pin::new(&mut b.transport).poll(cx).is_ready())).await;
    assert!(!more, "a second incoming connection");
    assert_eq!(b.refusals.counts().connections, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_beyond_its_stream_quota_is_reset() {
    // Two at once: the third is reset, and the two work.
    let quotas = KadQuotas {
        peer_streams: 2,
        ..KadQuotas::default()
    };
    let mut a = node(&agent(1)).await;
    let mut b = node_with(
        &agent(2),
        quotas,
        Arc::new(SystemClock),
        AddressBook::default(),
    )
    .await;
    let ((_, mut at_a), (_, at_b)) = connect(&mut a, &mut b).await;
    let mut substreams = accept_all(at_b);
    let mut first = open(&mut at_a, b"first").await;
    let mut first_in = accepted(&mut substreams).await;
    let mut second = open(&mut at_a, b"second").await;
    let mut second_in = accepted(&mut substreams).await;
    let mut third = open(&mut at_a, b"third").await;
    assert_refused(&mut third).await;
    // Refused, it is dropped, as libp2p does: A keeps no more of its own
    // open than it must.
    drop(third);
    assert_eq!(b.refusals.counts().streams, 1);
    answer(&mut first, &mut first_in, b"first").await;
    answer(&mut second, &mut second_in, b"second").await;

    // Every connection of the peer counts: one more, while it holds two.
    let ((_, mut again_at_a), (_, again_at_b)) = connect(&mut a, &mut b).await;
    let mut again = accept_all(again_at_b);
    let mut fourth = open(&mut again_at_a, b"fourth").await;
    assert_refused(&mut fourth).await;
    drop(fourth);
    assert_eq!(b.refusals.counts().streams, 2);

    // A substream dropped makes room for another, on either connection.
    drop(first_in);
    let mut fifth = open(&mut again_at_a, b"fifth").await;
    let mut fifth_in = accepted(&mut again).await;
    answer(&mut fifth, &mut fifth_in, b"fifth").await;

    // Three a minute of the node's clock: the fourth of the minute is reset,
    // and the next minute has room again.
    let clock = Arc::new(ManualClock::new(1_000_000));
    let quotas = KadQuotas {
        peer_streams_per_minute: 3,
        ..KadQuotas::default()
    };
    let mut c = node(&agent(3)).await;
    let mut d = node_with(&agent(4), quotas, clock.clone(), AddressBook::default()).await;
    let ((_, mut at_c), (_, at_d)) = connect(&mut c, &mut d).await;
    let mut substreams = accept_all(at_d);
    for message in [&b"one"[..], b"two", b"three"] {
        let mut outbound = open(&mut at_c, message).await;
        let mut inbound = accepted(&mut substreams).await;
        answer(&mut outbound, &mut inbound, message).await;
    }
    let mut fourth = open(&mut at_c, b"four").await;
    assert_refused(&mut fourth).await;
    assert_eq!(d.refusals.counts().streams, 1);
    clock.advance(59);
    let mut still = open(&mut at_c, b"still the same minute").await;
    assert_refused(&mut still).await;
    assert_eq!(d.refusals.counts().streams, 2);
    clock.advance(1);
    let mut next = open(&mut at_c, b"the next minute").await;
    let mut next_in = accepted(&mut substreams).await;
    answer(&mut next, &mut next_in, b"the next minute").await;
    assert_eq!(d.refusals.counts().streams, 2);
    assert_eq!(d.refusals.counts().connections, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn global_stream_cap_refuses_beyond_it() {
    let quotas = KadQuotas {
        global_streams: 2,
        ..KadQuotas::default()
    };
    let (mut a, mut c) = (node(&agent(1)).await, node(&agent(3)).await);
    let mut b = node_with(
        &agent(2),
        quotas,
        Arc::new(SystemClock),
        AddressBook::default(),
    )
    .await;
    let ((_, mut at_a), (_, a_at_b)) = connect(&mut a, &mut b).await;
    let ((_, mut at_c), (_, c_at_b)) = connect(&mut c, &mut b).await;
    let (mut from_a, mut from_c) = (accept_all(a_at_b), accept_all(c_at_b));

    let mut one = open(&mut at_a, b"one of A").await;
    let mut one_in = accepted(&mut from_a).await;
    let other = open(&mut at_c, b"one of C").await;
    let other_in = accepted(&mut from_c).await;
    // A has one of its own eight, but the node has two of two.
    let mut beyond = open(&mut at_a, b"beyond").await;
    assert_refused(&mut beyond).await;
    assert_eq!(b.refusals.counts().streams, 1);
    answer(&mut one, &mut one_in, b"one of A").await;

    // C's dropped: room for one more of anyone's.
    drop(other_in);
    drop(other);
    let mut again = open(&mut at_a, b"again").await;
    let mut again_in = accepted(&mut from_a).await;
    answer(&mut again, &mut again_in, b"again").await;
    assert_eq!(b.refusals.counts().streams, 1);
}

/// A waker that remembers it was woken.
struct Woken(AtomicBool);

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn outbound_streams_to_one_peer_wait_for_a_free_slot() {
    // B takes 8 streams at a time from a peer, its default: A keeps at
    // most half of that open of its own, over all its connections to B.
    let (mut a, mut b) = (node(&agent(1)).await, node(&agent(2)).await);
    let ((_, mut at_a), (_, at_b)) = connect(&mut a, &mut b).await;
    let ((_, mut again_at_a), (_, again_at_b)) = connect(&mut a, &mut b).await;
    let mut substreams = accept_all(at_b);
    let _again = accept_all(again_at_b);
    let mut open_four = Vec::new();
    for message in [&b"one"[..], b"two", b"three", b"four"] {
        let outbound = open(&mut at_a, message).await;
        let inbound = accepted(&mut substreams).await;
        open_four.push((outbound, inbound, message));
    }

    // The fifth waits, on either connection, and fails nothing.
    let woken = Arc::new(Woken(AtomicBool::new(false)));
    let waker = Waker::from(Arc::clone(&woken));
    let mut cx = Context::from_waker(&waker);
    assert!(at_a.poll_outbound_unpin(&mut cx).is_pending());
    assert!(again_at_a.poll_outbound_unpin(&mut cx).is_pending());
    assert!(!woken.0.load(Ordering::SeqCst));

    // One of the four closes: the waiting side is woken, and opens it.
    let (mut outbound, mut inbound, message) = open_four.remove(0);
    answer(&mut outbound, &mut inbound, message).await;
    drop(outbound);
    assert!(woken.0.load(Ordering::SeqCst), "not woken");
    let mut fifth = open(&mut at_a, b"five").await;
    let mut fifth_in = accepted(&mut substreams).await;
    answer(&mut fifth, &mut fifth_in, b"five").await;
    for (mut outbound, mut inbound, message) in open_four {
        answer(&mut outbound, &mut inbound, message).await;
    }
    assert_eq!(b.refusals.counts().streams, 0);
}

/// A UDP port of 127.0.0.1 that was free a moment ago.
fn free_udp_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_off_with_listen_binds_there() {
    let listen = SocketAddr::from((Ipv4Addr::LOCALHOST, free_udp_port()));
    let node = common::spawn_node_with(WorkerCfg::Echo, |cfg| {
        assert!(!cfg.net.relay);
        cfg.net.listen = Some(listen);
    })
    .await;
    let addrs: Vec<SocketAddr> = node.node.addr().ip_addrs().copied().collect();
    assert_eq!(addrs, vec![listen]);
    common::shut_down(node.node).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_off_without_listen_stays_on_loopback() {
    let node = common::spawn_node(WorkerCfg::Echo).await;
    let addr = node.node.addr();
    assert!(addr.relay_urls().next().is_none(), "{addr:?}");
    let addrs: Vec<SocketAddr> = addr.ip_addrs().copied().collect();
    assert_eq!(addrs.len(), 1, "{addrs:?}");
    assert_eq!(addrs[0].ip(), Ipv4Addr::LOCALHOST);
    assert_ne!(addrs[0].port(), 0);
    common::shut_down(node.node).await;
}

#[test]
fn listen_that_is_not_a_socket_address_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    for bad in [
        "not an address",
        "127.0.0.1",
        "4433",
        "localhost:4433",
        "127.0.0.1:70000",
    ] {
        std::fs::write(&path, format!("[net]\nlisten = \"{bad}\"\n")).unwrap();
        let error = Config::load(&path).expect_err(bad);
        assert!(format!("{error:#}").contains("listen"), "{bad}: {error:#}");
    }
    std::fs::write(&path, "[net]\nlisten = 4433\n").unwrap();
    assert!(Config::load(&path).is_err());

    // A socket address is taken, with the relay or without it; none by
    // default.
    std::fs::write(&path, "[net]\nrelay = false\nlisten = \"0.0.0.0:4433\"\n").unwrap();
    let cfg = Config::load(&path).unwrap();
    assert_eq!(cfg.net.listen, Some("0.0.0.0:4433".parse().unwrap()));
    std::fs::write(&path, "[net]\nlisten = \"[::]:4433\"\n").unwrap();
    let cfg = Config::load(&path).unwrap();
    assert!(cfg.net.relay);
    assert_eq!(cfg.net.listen, Some("[::]:4433".parse().unwrap()));
    std::fs::write(&path, "[net]\nrelay = false\n").unwrap();
    assert_eq!(Config::load(&path).unwrap().net.listen, None);
}
