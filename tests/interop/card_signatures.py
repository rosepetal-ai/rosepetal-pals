#!/usr/bin/env python3
"""An independent check of PALS Agent Card signatures with the official
Python SDK.

PALS signs its card with JWS ``EdDSA`` and the agent's own Ed25519 key,
whose public half, in hex, is the ``kid`` (``crates/pals-core/src/card_jws.rs``).
This script verifies such a card with ``a2a-sdk``'s own verifier,
``a2a.utils.signing.create_signature_verifier``, restricted to
``algorithms=["EdDSA"]``, and a key provider that trusts only the expected
agent: it returns the OKP/Ed25519 key whose ``x`` is the ``kid`` only when
the ``kid`` is that agent's id. Three checks:

1. the card verifies;
2. a copy with ``skills[0].description`` changed fails with
   ``InvalidSignaturesError``;
3. the card fails with ``InvalidSignaturesError`` when another agent is the
   expected one.

The card comes from one of:

- ``--vector PATH``: a frozen vector (``crates/pals-core/tests/vectors/card-jws-v1.json``),
  its ``signed_card`` and ``agent_id``;
- ``--gateway URL --peer ID``: the signed card of the peer ``ID`` as the
  local gateway at ``URL`` serves it, untouched, at
  ``/peers/<ID>/signed-agent-card.json``.

Prints ``3 checks passed`` and exits 0, or names the first difference and
exits 1. Needs the interop venv: ``a2a-sdk`` and ``PyJWT`` as pinned in
``requirements.txt``.
"""

from __future__ import annotations

import argparse
import base64
import copy
import json
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

import httpx
from a2a.client.card_resolver import parse_agent_card
from a2a.utils.signing import InvalidSignaturesError, create_signature_verifier
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
from jwt.api_jwk import PyJWK
from jwt.exceptions import PyJWKError

# Seconds the gateway may take to fetch and check the peer's card: the host
# may be loaded.
HTTP_TIMEOUT_S = 60.0


class CheckFailed(Exception):
    """A difference between what the SDK did and what PALS expects."""


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b'=').decode('ascii')


def key_provider_for(expected: str) -> Callable[[str | None, str | None], PyJWK]:
    """A key provider that trusts only ``expected``, an agent id in hex."""

    def key_provider(kid: str | None, jku: str | None) -> PyJWK:
        if kid != expected:
            raise PyJWKError(f'untrusted kid {kid!r}')
        return PyJWK({'kty': 'OKP', 'crv': 'Ed25519', 'x': b64url(bytes.fromhex(kid))})

    return key_provider


def verify(card_json: dict[str, Any], expected: str) -> None:
    """Raises ``InvalidSignaturesError`` unless ``expected`` signed the card."""
    verifier = create_signature_verifier(key_provider_for(expected), ['EdDSA'])
    verifier(parse_agent_card(copy.deepcopy(card_json)))


def another_agent_id() -> str:
    """The id of the agent whose secret key is 32 bytes of 0x02."""
    public = Ed25519PrivateKey.from_private_bytes(bytes([2] * 32)).public_key()
    return public.public_bytes(Encoding.Raw, PublicFormat.Raw).hex()


def checks(card_json: dict[str, Any], expected: str) -> int:
    verify(card_json, expected)
    print(f'check 1 passed: the card verifies with kid {expected}')

    tampered = copy.deepcopy(card_json)
    tampered['skills'][0]['description'] += ' (tampered)'
    try:
        verify(tampered, expected)
    except InvalidSignaturesError:
        print('check 2 passed: a changed skills[0].description is InvalidSignaturesError')
    else:
        raise CheckFailed('a card with a changed skills[0].description verified')

    other = another_agent_id()
    if other == expected:
        raise CheckFailed('the other agent id is the expected one')
    try:
        verify(card_json, other)
    except InvalidSignaturesError:
        print(f'check 3 passed: with {other} expected instead, InvalidSignaturesError')
    else:
        raise CheckFailed(f'the card verified with {other} expected')
    return 3


def load(args: argparse.Namespace) -> tuple[dict[str, Any], str]:
    if args.vector:
        vector = json.loads(Path(args.vector).read_text(encoding='utf-8'))
        return vector['signed_card'], vector['agent_id']
    url = f'{args.gateway.rstrip("/")}/peers/{args.peer}/signed-agent-card.json'
    response = httpx.get(url, timeout=HTTP_TIMEOUT_S, trust_env=False)
    response.raise_for_status()
    return response.json(), args.peer


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--vector', help='a frozen card vector (JSON)')
    source.add_argument('--gateway', help='base URL of a local PALS gateway')
    parser.add_argument('--peer', help='with --gateway: the agent id of the peer')
    args = parser.parse_args()
    if args.gateway and not args.peer:
        parser.error('--gateway needs --peer')

    try:
        card_json, expected = load(args)
        passed = checks(card_json, expected)
    except (CheckFailed, InvalidSignaturesError, httpx.HTTPError) as failed:
        print(f'FAIL: {type(failed).__name__}: {failed}')
        return 1
    print(f'{passed} checks passed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
