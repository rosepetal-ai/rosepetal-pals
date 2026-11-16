#!/usr/bin/env node
// Verifies a PALS Agent Card signature with the official JS SDK,
// @a2a-js/sdk (its verifyAgentCardSignature, over jose), as pinned in
// package-lock.json.
//
// Usage, after `npm ci` in this directory:
//
//   node tests/interop/card_signatures/js/verify.mjs <vector.json>
//   node tests/interop/card_signatures/js/verify.mjs <agent id> <signed-agent-card.json>
//
// The card is a frozen vector's signed_card, checked against its agent_id
// (crates/pals-core/tests/vectors/card-jws-v1.json), or a live card: the
// body of GET <gateway>/peers/<agent id>/signed-agent-card.json of a PALS
// gateway, saved to a file, checked against the agent id given. It is
// checked with a key retriever that trusts only that agent, returning the
// OKP/Ed25519 JWK whose x is the kid, and only for that kid.
// A copy with skills[0].description changed must fail, and so must the card
// when another agent is expected. Prints "verified" and exits 0, or names
// the difference and exits 1.

import { readFileSync } from 'node:fs';
import { verifyAgentCardSignature } from '@a2a-js/sdk';

// The id of the agent whose secret key is 32 bytes of 0x02.
const OTHER_AGENT_ID = '8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394';

// The SDK reports each failed signature with console.debug: kept, and shown
// only when the result is not the expected one.
const debug = [];
console.debug = (...args) => debug.push(args.map(String).join(' '));

function verifierFor(expected) {
  return verifyAgentCardSignature(async (kid) => {
    if (kid !== expected) {
      throw new Error(`untrusted kid ${kid}`);
    }
    return { kty: 'OKP', crv: 'Ed25519', x: Buffer.from(kid, 'hex').toString('base64url') };
  });
}

async function verifies(card, expected) {
  debug.length = 0;
  try {
    await verifierFor(expected)(structuredClone(card));
    return true;
  } catch {
    return false;
  }
}

function fail(what) {
  console.log(`FAIL: ${what}`);
  for (const line of debug) {
    console.log(`  ${line}`);
  }
  process.exit(1);
}

// The card to check and the agent expected to have signed it: a vector's
// signed_card and agent_id, or the agent id given and the card in the file
// given.
let card;
let expected;
if (process.argv.length === 3) {
  const vector = JSON.parse(readFileSync(process.argv[2], 'utf8'));
  card = vector.signed_card;
  expected = vector.agent_id;
} else if (process.argv.length === 4) {
  expected = process.argv[2];
  card = JSON.parse(readFileSync(process.argv[3], 'utf8'));
} else {
  fail(
    'usage: node tests/interop/card_signatures/js/verify.mjs <vector.json> | <agent id> <signed-agent-card.json>',
  );
}
if (!Array.isArray(card.skills) || card.skills.length === 0) {
  fail('the card has no skills[0] to change');
}

if (!(await verifies(card, expected))) {
  fail('the card does not verify');
}

const tampered = structuredClone(card);
tampered.skills[0].description += ' (tampered)';
if (await verifies(tampered, expected)) {
  fail('a card with a changed skills[0].description verified');
}

if (OTHER_AGENT_ID === expected) {
  fail('the other agent id is the expected one');
}
if (await verifies(card, OTHER_AGENT_ID)) {
  fail(`the card verified with ${OTHER_AGENT_ID} expected`);
}

console.log('verified');
