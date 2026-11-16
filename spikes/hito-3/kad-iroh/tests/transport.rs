//! The libp2p transport made of Iroh connections, on its own: two
//! endpoints on loopback, each with an agent's key, dialing and accepting
//! through `IrohTransport` as a libp2p swarm would.

use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::Pin;

use iroh::protocol::Router;
use libp2p::Multiaddr;
use libp2p::core::muxing::{StreamMuxerBox, StreamMuxerExt};
use libp2p::core::transport::{DialOpts, ListenerId, PortUse, TransportEvent};
use libp2p::core::{Endpoint, Transport};
use libp2p::futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::identity::{self, PeerId};
use libp2p::multiaddr::Protocol;
use pals_core::identity::AgentKey;
use spike_kad_iroh::bind_endpoint;
use spike_kad_iroh::transport::{self, ALPN_KAD, IrohTransport};

/// One node: its transport, listening, and the router that hands it the
/// connections of [`ALPN_KAD`].
struct Node {
    transport: IrohTransport,
    /// Where it listens, `/ip4/127.0.0.1/udp/<port>/quic-v1`.
    addr: Multiaddr,
    peer: PeerId,
    _router: Router,
}

fn agent(seed: u8) -> AgentKey {
    AgentKey::from_bytes(&[seed; 32])
}

/// The `PeerId` of `agent`'s own Ed25519 public key, as libp2p makes it.
fn agent_peer_id(agent: &AgentKey) -> PeerId {
    let key = identity::ed25519::PublicKey::try_from_bytes(agent.id().as_bytes()).unwrap();
    identity::PublicKey::from(key).to_peer_id()
}

async fn node(agent: &AgentKey) -> Node {
    let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let endpoint = bind_endpoint(listen, agent).await.unwrap();
    let (mut transport, listener) = IrohTransport::new(endpoint.clone());
    let router = Router::builder(endpoint).accept(ALPN_KAD, listener).spawn();
    transport
        .listen_on(ListenerId::next(), transport::quic_addr(listen))
        .unwrap();
    let addr = match next_event(&mut transport).await {
        TransportEvent::NewAddress { listen_addr, .. } => listen_addr,
        other => panic!("not a new address: {other:?}"),
    };
    Node {
        transport,
        addr,
        peer: agent_peer_id(agent),
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
        role: Endpoint::Dialer,
        port_use: PortUse::Reuse,
    }
}

/// `from` dials `to`, which accepts: what each side gets, the `PeerId` it
/// sees and the connection.
async fn connect(
    from: &mut Node,
    to: &mut Node,
) -> ((PeerId, StreamMuxerBox), (PeerId, StreamMuxerBox)) {
    let addr = to.addr.clone().with(Protocol::P2p(to.peer));
    let dial = from.transport.dial(addr, dialer()).unwrap();
    let accept = async {
        loop {
            if let TransportEvent::Incoming { upgrade, .. } = next_event(&mut to.transport).await {
                break upgrade.await;
            }
        }
    };
    let (dialed, accepted) = tokio::join!(dial, accept);
    (dialed.unwrap(), accepted.unwrap())
}

/// `opener` opens a substream and sends `message`; `acceptor` takes the
/// substream, reads it and answers with it backwards.
async fn round_trip(opener: &mut StreamMuxerBox, acceptor: &mut StreamMuxerBox, message: &[u8]) {
    let mut outbound = poll_fn(|cx| opener.poll_outbound_unpin(cx)).await.unwrap();
    outbound.write_all(message).await.unwrap();
    outbound.flush().await.unwrap();

    let mut inbound = poll_fn(|cx| acceptor.poll_inbound_unpin(cx)).await.unwrap();
    let mut received = vec![0; message.len()];
    inbound.read_exact(&mut received).await.unwrap();
    assert_eq!(received, message);
    let answer: Vec<u8> = message.iter().rev().copied().collect();
    inbound.write_all(&answer).await.unwrap();
    inbound.close().await.unwrap();

    outbound.close().await.unwrap();
    let mut back = Vec::new();
    outbound.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, answer);
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
    assert_eq!(bob_seen, agent_peer_id(&bob));
    assert_eq!(alice_seen, agent_peer_id(&alice));
    // The transport's own mapping says the same.
    let bob_endpoint = iroh::PublicKey::from_bytes(bob.id().as_bytes()).unwrap();
    assert_eq!(transport::peer_id(&bob_endpoint), agent_peer_id(&bob));
    assert_eq!(transport::endpoint_id(&bob_seen), Some(bob_endpoint));
}

#[tokio::test(flavor = "multi_thread")]
async fn dial_with_a_peer_id_not_matching_the_endpoint_fails() {
    let (mut a, b) = (node(&agent(1)).await, node(&agent(2)).await);
    // B's address with C's PeerId: Iroh's TLS finds B's key, not C's.
    let wrong = b.addr.clone().with(Protocol::P2p(agent_peer_id(&agent(3))));
    let error = a
        .transport
        .dial(wrong, dialer())
        .unwrap()
        .await
        .unwrap_err();
    assert!(
        matches!(error, transport::Error::Connect(_)),
        "refused by Iroh's handshake, not by a timeout: {error:?}"
    );
    // An address with no PeerId names no Iroh endpoint.
    assert!(a.transport.dial(b.addr.clone(), dialer()).is_err());
}
