#!/usr/bin/env python3
"""F, the stock A2A agent of the hito 2 exit test
(docs/runbooks/hito-2-consulta-de-servicios.md, case 7).

An A2A 1.0 agent built only on the official Python SDK, ``a2a-sdk`` as
pinned in ``tests/interop/requirements.txt``, with the SDK's own Starlette
routes (``a2a-sdk[http-server]``) served by uvicorn, pinned in
``tests/exit/hito-2/requirements-f.txt``. No PALS code: it is the kind of
agent a PALS operator saves with ``pals contact add-a2a``.

It serves, at ``--url`` (its base URL, the origin the PALS contact keeps):

- its card at ``/.well-known/agent-card.json``, with a JSON-RPC interface
  at ``<url>/jsonrpc`` and an HTTP+JSON one at ``<url>/rest``, protocol 1.0;
- with ``--sign-key``, the card signed with the SDK's own signer
  (``a2a.utils.signing.create_agent_card_signer``): JWS ``EdDSA`` with an
  Ed25519 key, ``kid`` ``f-1`` and ``jku`` ``<url>/jwks.json``, where the
  JWK Set is served; the key is created in that file if it does not exist;
- with ``--tamper`` too, the card signed and then changed (its
  ``description``), as an agent whose card was altered after signing.

Every message is answered with a completed task whose artifact is the text
``F: <the message's text>``. Over HTTPS with ``--tls-cert`` and
``--tls-key``; plain HTTP otherwise.
"""

from __future__ import annotations

import argparse
import base64
import os
import tempfile
from pathlib import Path

import uvicorn
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import (
    Encoding,
    NoEncryption,
    PrivateFormat,
    PublicFormat,
    load_pem_private_key,
)
from starlette.applications import Starlette
from starlette.responses import JSONResponse
from starlette.routing import Route

from a2a.helpers.proto_helpers import new_task_from_user_message, new_text_part
from a2a.server.agent_execution import AgentExecutor, RequestContext
from a2a.server.events import EventQueue
from a2a.server.request_handlers import DefaultRequestHandler
from a2a.server.routes import (
    create_agent_card_routes,
    create_jsonrpc_routes,
    create_rest_routes,
)
from a2a.server.tasks import InMemoryTaskStore, TaskUpdater
from a2a.types import (
    AgentCapabilities,
    AgentCard,
    AgentInterface,
    AgentSkill,
)
from a2a.utils.constants import PROTOCOL_VERSION_1_0, TransportProtocol
from a2a.utils.signing import create_agent_card_signer

KID = 'f-1'


class Echo(AgentExecutor):
    """Answers every message with ``F: <its text>``, as a completed task."""

    async def execute(self, context: RequestContext, event_queue: EventQueue) -> None:
        task = context.current_task or new_task_from_user_message(context.message)
        if context.current_task is None:
            await event_queue.enqueue_event(task)
        updater = TaskUpdater(event_queue, task.id, task.context_id)
        await updater.add_artifact([new_text_part(f'F: {context.get_user_input()}')], name='respuesta')
        await updater.complete()

    async def cancel(self, context: RequestContext, event_queue: EventQueue) -> None:
        task = context.current_task
        if task is not None:
            await TaskUpdater(event_queue, task.id, task.context_id).cancel()


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b'=').decode('ascii')


def signing_key(path: Path) -> Ed25519PrivateKey:
    """The Ed25519 key in ``path``, created there first if need be.

    Two F processes may start at once on the same file: the key is written
    whole to a temporary file of the same directory and linked into place,
    which fails if another process linked its own first. Either way, the
    file is read only once it is complete, so both use the same key.
    """
    if not path.exists():
        pem = Ed25519PrivateKey.generate().private_bytes(
            Encoding.PEM, PrivateFormat.PKCS8, NoEncryption()
        )
        fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f'.{path.name}.')
        try:
            os.write(fd, pem)
            os.fchmod(fd, 0o600)
            os.close(fd)
            try:
                os.link(tmp, path)
            except FileExistsError:
                pass
        finally:
            os.unlink(tmp)
    key = load_pem_private_key(path.read_bytes(), password=None)
    if not isinstance(key, Ed25519PrivateKey):
        raise SystemExit(f'{path} is not an Ed25519 key')
    return key


def card_for(url: str) -> AgentCard:
    return AgentCard(
        name='Agente F',
        description='Agente A2A de serie (SDK oficial de Python) de la prueba de salida del hito 2',
        version='1.0.0',
        supported_interfaces=[
            AgentInterface(
                url=f'{url}/jsonrpc',
                protocol_binding=TransportProtocol.JSONRPC,
                protocol_version=PROTOCOL_VERSION_1_0,
            ),
            AgentInterface(
                url=f'{url}/rest',
                protocol_binding=TransportProtocol.HTTP_JSON,
                protocol_version=PROTOCOL_VERSION_1_0,
            ),
        ],
        capabilities=AgentCapabilities(streaming=True),
        default_input_modes=['text/plain'],
        default_output_modes=['text/plain'],
        skills=[
            AgentSkill(
                id='eco',
                name='Eco',
                description='Devuelve el texto recibido',
                tags=['example:eco'],
            )
        ],
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('--listen', default='0.0.0.0:8443', help='HOST:PORT to listen on')
    parser.add_argument('--url', required=True, help='the base URL peers reach it at')
    parser.add_argument('--tls-cert', help='the server certificate (PEM), for HTTPS')
    parser.add_argument('--tls-key', help='its private key (PEM)')
    parser.add_argument('--sign-key', type=Path, help='sign the card with the Ed25519 key in this file')
    parser.add_argument('--tamper', action='store_true', help='change the card after signing it')
    args = parser.parse_args()
    if args.tamper and not args.sign_key:
        parser.error('--tamper needs --sign-key')
    url = args.url.rstrip('/')

    card = card_for(url)
    extra = []
    if args.sign_key:
        key = signing_key(args.sign_key)
        signer = create_agent_card_signer(
            key, {'alg': 'EdDSA', 'kid': KID, 'jku': f'{url}/jwks.json', 'typ': 'JOSE'}
        )
        card = signer(card)
        x = key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
        jwks = {'keys': [{'kty': 'OKP', 'crv': 'Ed25519', 'kid': KID, 'x': b64url(x)}]}

        async def jwks_route(_request):
            return JSONResponse(jwks)

        extra.append(Route('/jwks.json', jwks_route, methods=['GET']))
    if args.tamper:
        card.description = card.description + ' (alterada después de firmarla)'

    handler = DefaultRequestHandler(
        agent_executor=Echo(), task_store=InMemoryTaskStore(), agent_card=card
    )
    routes = [
        *create_agent_card_routes(card),
        *create_jsonrpc_routes(handler, rpc_url='/jsonrpc'),
        *create_rest_routes(handler, path_prefix='/rest'),
        *extra,
    ]
    host, _, port = args.listen.rpartition(':')
    uvicorn.run(
        Starlette(routes=routes),
        host=host,
        port=int(port),
        ssl_certfile=args.tls_cert,
        ssl_keyfile=args.tls_key,
        log_level='info',
    )


if __name__ == '__main__':
    main()
