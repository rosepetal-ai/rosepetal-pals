//! The node contract's verification of what a backend's search returns:
//! each candidate's contact and announce records, from `pals-core`, checked
//! against the capability searched and the clock, and deduplicated by agent.

use std::net::SocketAddr;

use pals_core::identity::AgentKey;
use pals_core::records::{AnnounceRecord, ContactRecord};
use pals_core::signed::Signed;
use spike_common::{RawCandidate, dedupe, issue_announce, issue_contact, tamper_signature, verify};

const OCR: &str = "agentnet:capability:v1:vision.ocr";
const REVIEW: &str = "agentnet:capability:v1:code.rust.review";
/// When the records are issued.
const ISSUED: i64 = 1_800_000_000;
/// When they are verified: inside their 24 h.
const NOW: i64 = ISSUED + 600;

fn key(seed: u8) -> AgentKey {
    AgentKey::from_bytes(&[seed; 32])
}

fn contact(key: &AgentKey) -> Signed<ContactRecord> {
    contact_at(key, "192.0.2.10:4433", 1)
}

fn contact_at(key: &AgentKey, addr: &str, seq: u64) -> Signed<ContactRecord> {
    let addr: SocketAddr = addr.parse().unwrap();
    issue_contact(key, &[addr], seq, ISSUED).unwrap()
}

fn announce(key: &AgentKey, capabilities: &[&str]) -> Signed<AnnounceRecord> {
    let capabilities: Vec<String> = capabilities.iter().map(|c| c.to_string()).collect();
    issue_announce(key, &capabilities, 1, ISSUED).unwrap()
}

fn raw(contact: &Signed<ContactRecord>, announce: &Signed<AnnounceRecord>) -> RawCandidate {
    RawCandidate {
        contact: Some(contact.to_json()),
        announce: Some(announce.to_json()),
        found_ms: None,
    }
}

#[test]
fn verify_accepts_a_matching_pair() {
    let a = key(1);
    let candidate = verify(OCR, raw(&contact(&a), &announce(&a, &[OCR, REVIEW])), NOW);
    assert!(candidate.valid);
    assert_eq!(candidate.reason, None);
    assert_eq!(candidate.agent_id, Some(a.id()));
    assert_eq!(candidate.direct_addrs, ["192.0.2.10:4433"]);
    assert_eq!(candidate.contact_seq, Some(1));
}

#[test]
fn verify_rejects_a_tampered_contact() {
    let a = key(1);
    // What `--rogue` publishes.
    let rogue = tamper_signature(&contact(&a));
    assert_ne!(rogue.signature, contact(&a).signature);
    let candidate = verify(OCR, raw(&rogue, &announce(&a, &[OCR])), NOW);
    assert!(!candidate.valid);
    assert_eq!(candidate.reason, Some("bad_contact"));
    // Named by its announce, which verifies.
    assert_eq!(candidate.agent_id, Some(a.id()));
    assert!(candidate.direct_addrs.is_empty());
}

#[test]
fn verify_rejects_a_missing_record() {
    let a = key(1);
    let no_contact = RawCandidate {
        contact: None,
        announce: Some(announce(&a, &[OCR]).to_json()),
        found_ms: None,
    };
    assert_eq!(verify(OCR, no_contact, NOW).reason, Some("bad_contact"));
    let no_announce = RawCandidate {
        contact: Some(contact(&a).to_json()),
        announce: None,
        found_ms: None,
    };
    assert_eq!(verify(OCR, no_announce, NOW).reason, Some("bad_announce"));
}

#[test]
fn verify_rejects_mixed_agents() {
    let (a, b) = (key(1), key(2));
    let candidate = verify(OCR, raw(&contact(&a), &announce(&b, &[OCR])), NOW);
    assert!(!candidate.valid);
    assert_eq!(candidate.reason, Some("agent_mismatch"));
}

#[test]
fn verify_rejects_a_missing_capability() {
    let a = key(1);
    let candidate = verify(OCR, raw(&contact(&a), &announce(&a, &[REVIEW])), NOW);
    assert!(!candidate.valid);
    assert_eq!(candidate.reason, Some("capability_missing"));
}

#[test]
fn verify_rejects_records_out_of_their_window() {
    let a = key(1);
    let pair = raw(&contact(&a), &announce(&a, &[OCR]));
    let later = ISSUED + 25 * 3600;
    assert_eq!(verify(OCR, pair, later).reason, Some("expired"));
    let pair = raw(&contact(&a), &announce(&a, &[OCR]));
    let earlier = ISSUED - 3600;
    assert_eq!(verify(OCR, pair, earlier).reason, Some("expired"));
}

#[test]
fn candidates_dedupe_by_agent() {
    let (a, b) = (key(1), key(2));
    let found = vec![
        // A twice, the first copy with a contact that does not verify.
        verify(
            OCR,
            raw(&tamper_signature(&contact(&a)), &announce(&a, &[OCR])),
            NOW,
        ),
        verify(OCR, raw(&contact(&b), &announce(&b, &[OCR])), NOW),
        verify(OCR, raw(&contact(&a), &announce(&a, &[OCR])), NOW),
    ];
    let candidates = dedupe(found);
    assert_eq!(candidates.len(), 2);
    // In the order each agent first appeared, with the first valid copy.
    assert_eq!(candidates[0].agent_id, Some(a.id()));
    assert!(candidates[0].valid);
    assert_eq!(candidates[1].agent_id, Some(b.id()));
    assert!(candidates[1].valid);
}

#[test]
fn dedupe_keeps_an_agent_with_no_valid_copy_once() {
    let a = key(1);
    let rogue = || {
        verify(
            OCR,
            raw(&tamper_signature(&contact(&a)), &announce(&a, &[OCR])),
            NOW,
        )
    };
    let candidates = dedupe(vec![rogue(), rogue()]);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].reason, Some("bad_contact"));
}

#[test]
fn dedupe_keeps_the_newest_valid_contact() {
    let a = key(1);
    let ocr = announce(&a, &[OCR]);
    let check = |addr, seq| verify(OCR, raw(&contact_at(&a, addr, seq), &ocr), NOW);
    // The second copy has the higher seq: it wins, though it came later.
    let newest = dedupe(vec![
        check("192.0.2.10:4433", 1),
        check("192.0.2.11:4433", 2),
    ]);
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].contact_seq, Some(2));
    assert_eq!(newest[0].direct_addrs, ["192.0.2.11:4433"]);
    // On a tie, the first stays.
    let tie = dedupe(vec![
        check("192.0.2.10:4433", 3),
        check("192.0.2.11:4433", 3),
    ]);
    assert_eq!(tie[0].direct_addrs, ["192.0.2.10:4433"]);
    // An older valid copy after a newer one does not replace it.
    let older = dedupe(vec![
        check("192.0.2.11:4433", 5),
        check("192.0.2.10:4433", 4),
    ]);
    assert_eq!(older[0].contact_seq, Some(5));
}
