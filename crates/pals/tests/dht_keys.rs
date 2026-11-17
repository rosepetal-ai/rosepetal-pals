//! The DHT's identity and keys, frozen by `kad-v0.json`: the agent's
//! `PeerId`, the keys of its two values, the key of a capability's provider
//! records and the two address forms. The capability vocabulary's document
//! quotes every key, and the protocol's quotes the identity and addresses.

use std::str::FromStr;

use libp2p::kad::RecordKey;
use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};
use pals::dht::keys::{
    ANNOUNCE_KEY_PREFIX, CONTACT_KEY_PREFIX, Slot, agent_of, announce_key, contact_key, peer_id,
    provider_key, slot,
};
use pals_core::identity::{AgentId, AgentKey};
use serde_json::Value;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn vector() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../pals-core/tests/vectors/kad-v0.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen vector")).unwrap()
}

fn doc(path: &str) -> String {
    let path = format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).expect("the document")
}

fn text<'a>(vector: &'a Value, field: &str) -> &'a str {
    vector[field].as_str().expect(field)
}

/// The vector's agent: the one whose secret key is its `signing_key_hex`.
fn agent(vector: &Value) -> AgentId {
    let secret: [u8; 32] = unhex(text(vector, "signing_key_hex")).try_into().unwrap();
    let agent = AgentKey::from_bytes(&secret).id();
    assert_eq!(agent.to_string(), text(vector, "agent_id"));
    agent
}

#[test]
fn peer_id_is_the_agent_key() {
    let vector = vector();
    let agent = agent(&vector);
    let peer = peer_id(&agent);
    assert_eq!(peer.to_base58(), text(&vector, "peer_id"));
    assert_eq!(hex(&peer.to_bytes()), text(&vector, "peer_id_hex"));
    assert_eq!(agent_of(&peer), Some(agent));
}

#[test]
fn a_peer_id_of_another_kind_is_no_agent() {
    assert_eq!(agent_of(&PeerId::random()), None);

    // The agent's key under the identity multihash, but its protobuf with
    // the two fields the other way round: libp2p decodes it to the same
    // key, and it is still not the agent's one spelling.
    let vector = vector();
    let key = agent(&vector);
    let mut bytes = vec![0x00, 0x24, 0x12, 0x20];
    bytes.extend_from_slice(key.as_bytes());
    bytes.extend_from_slice(&[0x08, 0x01]);
    let decoded = libp2p::identity::PublicKey::try_decode_protobuf(&bytes[2..])
        .expect("libp2p reads the fields in any order");
    assert_eq!(decoded.to_peer_id(), peer_id(&key));
    let reordered = PeerId::from_bytes(&bytes).expect("an identity multihash");
    assert_ne!(reordered, peer_id(&key));
    assert_eq!(agent_of(&reordered), None);
    let key = RecordKey::new(&format!("{CONTACT_KEY_PREFIX}{reordered}"));
    assert_eq!(slot(&key), None);
}

#[test]
fn record_keys_match_the_vector() {
    let vector = vector();
    let agent = agent(&vector);
    let peer = text(&vector, "peer_id");
    assert_eq!(
        text(&vector, "contact_key"),
        format!("/agentnet/contact/{peer}")
    );
    assert_eq!(
        text(&vector, "announce_key"),
        format!("/agentnet/announce/{peer}")
    );
    assert_eq!(CONTACT_KEY_PREFIX, "/agentnet/contact/");
    assert_eq!(ANNOUNCE_KEY_PREFIX, "/agentnet/announce/");
    assert_eq!(
        contact_key(&agent).as_ref(),
        text(&vector, "contact_key").as_bytes()
    );
    assert_eq!(
        announce_key(&agent).as_ref(),
        text(&vector, "announce_key").as_bytes()
    );
}

#[test]
fn provider_keys_hash_the_full_capability_string() {
    let vector = vector();
    let keys = vector["provider_keys"].as_array().unwrap();
    // The four of the common vocabulary's v1 and a namespaced one; a
    // capability added to the vocabulary adds its entry.
    let capabilities: Vec<&str> = keys.iter().map(|e| text(e, "capability")).collect();
    for capability in [
        "vision.ocr",
        "vision.defect-detection",
        "code.rust.review",
        "knowledge.documentation",
        "example:metal-plates",
    ] {
        assert!(capabilities.contains(&capability), "{capability}");
    }
    for entry in keys {
        let capability = text(entry, "capability");
        let key = provider_key(capability).expect(capability);
        assert_eq!(hex(key.as_ref()), text(entry, "key_hex"), "{capability}");
        assert_eq!(
            pals_core::inventory::capability_tag(capability).unwrap(),
            text(entry, "string"),
            "{capability}"
        );
    }

    // One spelling per capability: never its full tag, in any case.
    for full in [
        "agentnet:capability:v1:vision.ocr",
        "AGENTNET:capability:v1:vision.ocr",
        "Agentnet:capability:v1:vision.ocr",
        "agentnet:vision.ocr",
        " agentnet:capability:v1:vision.ocr",
    ] {
        assert!(provider_key(full).is_err(), "{full:?}");
    }
    for invalid in ["", "Vision.OCR", "vision ocr", "example:", ":ocr"] {
        assert!(provider_key(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn slot_reads_only_canonical_keys() {
    let vector = vector();
    let agent = agent(&vector);
    let peer = text(&vector, "peer_id");
    let key = |text: String| RecordKey::new(&text);

    assert_eq!(
        slot(&key(format!("/agentnet/contact/{peer}"))),
        Some(Slot::Contact(agent))
    );
    assert_eq!(
        slot(&key(format!("/agentnet/announce/{peer}"))),
        Some(Slot::Announce(agent))
    );
    assert_eq!(slot(&contact_key(&agent)), Some(Slot::Contact(agent)));
    assert_eq!(slot(&announce_key(&agent)), Some(Slot::Announce(agent)));

    assert_eq!(slot(&key(format!("/agentnet/link/{peer}"))), None);
    assert_eq!(slot(&key(format!("/agentnet/contact/{peer}/"))), None);
    assert_eq!(slot(&key(format!("/agentnet/contact//{peer}"))), None);
    assert_eq!(slot(&key(format!("/agentnet/contact/ {peer}"))), None);
    assert_eq!(slot(&key("/agentnet/contact/".to_owned())), None);
    assert_eq!(
        slot(&key(format!("/agentnet/contact/{}", PeerId::random()))),
        None
    );
    assert_eq!(
        slot(&key(format!(
            "/agentnet/contact/{}",
            text(&vector, "agent_id")
        ))),
        None
    );
    assert_eq!(slot(&provider_key("vision.ocr").unwrap()), None);
}

#[test]
fn addresses_match_the_vector() {
    let vector = vector();
    let peer = peer_id(&agent(&vector));

    // A router: an Iroh endpoint at that UDP address.
    let router = Multiaddr::empty()
        .with(Protocol::Ip4([192, 0, 2, 10].into()))
        .with(Protocol::Udp(4433))
        .with(Protocol::QuicV1)
        .with(Protocol::P2p(peer));
    assert_eq!(router.to_string(), vector["addresses"]["router"]);
    assert_eq!(hex(&router.to_vec()), vector["addresses_hex"]["router"]);

    // A client: no location at all.
    let client = Multiaddr::empty().with(Protocol::P2p(peer));
    assert_eq!(client.to_string(), vector["addresses"]["client"]);
    assert_eq!(hex(&client.to_vec()), vector["addresses_hex"]["client"]);
    assert_eq!(
        Multiaddr::from_str(vector["addresses"]["client"].as_str().unwrap()).unwrap(),
        client
    );
}

#[test]
fn capabilities_doc_quotes_the_vector() {
    let vector = vector();
    let doc = doc("docs/extensions/capabilities-v1.md");
    for entry in vector["provider_keys"].as_array().unwrap() {
        for field in ["capability", "string", "key_hex"] {
            let value = text(entry, field);
            assert!(doc.contains(&format!("`{value}`")), "{value} is not quoted");
        }
    }
}

#[test]
fn kad_doc_quotes_the_vector() {
    let vector = vector();
    let doc = doc("docs/protocols/agentnet-kad-0.md");
    let mut quoted = vec![
        text(&vector, "agent_id"),
        text(&vector, "peer_id"),
        text(&vector, "peer_id_hex"),
        text(&vector, "contact_key"),
        text(&vector, "announce_key"),
        text(&vector, "alpn"),
        text(&vector, "protocol"),
    ];
    for form in ["router", "client"] {
        quoted.push(vector["addresses"][form].as_str().unwrap());
    }
    for value in quoted {
        assert!(doc.contains(&format!("`{value}`")), "{value} is not quoted");
    }
}
