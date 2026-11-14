#!/usr/bin/env python3
"""An independent check of PALS's A2A interface with the official Python SDK.

Starts three PALS daemons on this machine, in temporary directories, with
Iroh on 127.0.0.1 and no relay:

- ``echo``: a provider with the ``echo`` worker adapter;
- ``slow``: a provider with the ``tck`` adapter, whose
  ``test-resubscribe-message-id`` task stays ``WORKING`` for
  ``2 x TCK_STREAMING_TIMEOUT`` (here 2 x 60 s): an echo task completes at
  once, so it cannot be canceled;
- ``requester``: whose loopback gateway the SDK talks to.

They exchange contacts and grants with the ``pals`` CLI. Then, with
``a2a-sdk`` (the version pinned in ``requirements.txt``), the card of each
provider is resolved from the gateway, and four checks run, each over both
bindings the card declares, JSON-RPC and HTTP+JSON:

1. ``SendMessage`` with ``hola``: the task completes with ``echo: hola``.
2. The same in streaming: the last event's state is
   ``TASK_STATE_COMPLETED``.
3. ``GetTask`` of the task of check 1: completed, with ``echo: hola``.
4. A long task, canceled with ``CancelTask``: ``TASK_STATE_CANCELED``, and
   so it stays.

Prints ``4 checks passed`` and exits 0, or names the first difference and
exits 1. The daemons' logs stay in ``target/interop-run/<UTC time>/``.

Environment: ``PALS_BIN``, a pals binary to use instead of building one.
"""

from __future__ import annotations

import asyncio
import os
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from contextlib import ExitStack
from pathlib import Path

import httpx
from a2a.client import A2ACardResolver, ClientConfig, ClientFactory
from a2a.helpers.proto_helpers import get_artifact_text, new_text_message
from a2a.types import (
    AgentCard,
    CancelTaskRequest,
    GetTaskRequest,
    Role,
    SendMessageConfiguration,
    SendMessageRequest,
    Task,
    TaskState,
)
from a2a.utils.constants import TransportProtocol

ROOT = Path(__file__).resolve().parents[2]
BINDINGS = [TransportProtocol.JSONRPC.value, TransportProtocol.HTTP_JSON.value]
# The slow provider's task waits twice this, in seconds.
SLOW_TIMEOUT_S = '60'
# Seconds an HTTP exchange with the gateway may take: the host may be loaded.
HTTP_TIMEOUT_S = 60.0


class CheckFailed(Exception):
    """A difference between what PALS answered and what A2A expects."""


def expect(condition: bool, what: str) -> None:
    if not condition:
        raise CheckFailed(what)


# --- The daemons -------------------------------------------------------------


def pals_binary() -> Path:
    if binary := os.environ.get('PALS_BIN'):
        return Path(binary)
    subprocess.run(
        ['cargo', 'build', '--locked', '--quiet', '-p', 'pals', '--bin', 'pals'],
        cwd=ROOT,
        check=True,
    )
    return ROOT / 'target' / 'debug' / 'pals'


def free_port() -> int:
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


class Daemons:
    """The three daemons, their homes and the CLI that runs against them."""

    def __init__(self, stack: ExitStack, pals: Path, logs: Path) -> None:
        self.pals = pals
        self.logs = logs
        self.work = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='pals-interop.')))
        self.processes: list[subprocess.Popen[bytes]] = []
        stack.callback(self.stop)
        self.gateways: dict[str, int] = {}

    def cli(self, name: str, *args: str) -> str:
        """``pals <args> --home <name's home>``; its stdout."""
        done = subprocess.run(
            [str(self.pals), *args, '--home', str(self.work / name)],
            capture_output=True,
            text=True,
            check=False,
        )
        if done.returncode != 0:
            raise RuntimeError(f'pals {" ".join(args)} ({name}) failed: {done.stderr.strip()}')
        return done.stdout.strip()

    def start(self, name: str, adapter: str, env: dict[str, str] | None = None) -> None:
        home = self.work / name
        home.mkdir()
        port = free_port()
        (home / 'config.toml').write_text(
            f'[agent]\nname = "pals-interop-{name}"\n'
            f'description = "PALS agent for the Python SDK check ({adapter} adapter)"\n\n'
            f'[gateway]\nlisten = "127.0.0.1:{port}"\n\n'
            f'[worker]\nadapter = "{adapter}"\n\n'
            '[net]\nrelay = false\n'
        )
        self.cli(name, 'init')
        log = (self.logs / f'{name}.log').open('wb')
        process = subprocess.Popen(
            [str(self.pals), 'daemon', '--home', str(home)],
            stdout=log,
            stderr=subprocess.STDOUT,
            env={**os.environ, **(env or {})},
        )
        self.processes.append(process)
        self.gateways[name] = port
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(f'the {name} daemon exited, see {self.logs / name}.log')
            try:
                self.cli(name, 'id', '--ticket')
                return
            except RuntimeError:
                time.sleep(0.5)
        raise RuntimeError(f'the {name} daemon did not start within 60 s')

    def connect(self, requester: str, provider: str) -> str:
        """Contacts both ways and a grant from ``provider`` to
        ``requester``; the provider's agent id."""
        self.cli(requester, 'contact', 'add', self.cli(provider, 'id', '--ticket'), '--name', provider)
        self.cli(provider, 'contact', 'add', self.cli(requester, 'id', '--ticket'), '--name', requester)
        token = self.cli(
            provider, 'grant', 'issue', '--to', requester, '--project', 'interop',
            '--ops', 'task.send,task.read,task.cancel', '--ttl', '2h',
        )
        self.cli(requester, 'grant', 'import', token)
        return self.cli(provider, 'id')

    def stop(self) -> None:
        for process in self.processes:
            process.terminate()
        for process in self.processes:
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()


# --- The checks --------------------------------------------------------------


def hola() -> SendMessageRequest:
    return SendMessageRequest(message=new_text_message('hola', role=Role.ROLE_USER))


def answer_of(task: Task) -> str:
    return '\n'.join(get_artifact_text(artifact) for artifact in task.artifacts)


def state_name(state: int) -> str:
    return TaskState.Name(state)


class Checks:
    def __init__(self, http: httpx.AsyncClient, echo_card: AgentCard, slow_card: AgentCard) -> None:
        self.http = http
        self.echo_card = echo_card
        self.slow_card = slow_card
        # Check 1's task on each binding, for check 3.
        self.sent: dict[str, Task] = {}

    def client(self, card: AgentCard, binding: str, *, streaming: bool):
        config = ClientConfig(
            streaming=streaming,
            httpx_client=self.http,
            supported_protocol_bindings=[binding],
        )
        return ClientFactory(config).create(card)

    async def send(self, binding: str) -> None:
        client = self.client(self.echo_card, binding, streaming=False)
        responses = [response async for response in client.send_message(hola())]
        expect(len(responses) == 1, f'{len(responses)} responses to SendMessage')
        expect(responses[0].HasField('task'), f'SendMessage answered no task: {responses[0]}')
        task = responses[0].task
        expect(
            task.status.state == TaskState.TASK_STATE_COMPLETED,
            f'the task is {state_name(task.status.state)}',
        )
        expect(answer_of(task) == 'echo: hola', f'the answer is {answer_of(task)!r}')
        self.sent[binding] = task

    async def stream(self, binding: str) -> None:
        client = self.client(self.echo_card, binding, streaming=True)
        events = [event async for event in client.send_message(hola())]
        expect(bool(events), 'the stream had no events')
        expect(events[0].HasField('task'), f'the stream began with {events[0]}')
        last = events[-1]
        if last.HasField('status_update'):
            state = last.status_update.status.state
        elif last.HasField('task'):
            state = last.task.status.state
        else:
            raise CheckFailed(f'the stream ended with {last}')
        expect(
            state == TaskState.TASK_STATE_COMPLETED,
            f'the last state is {state_name(state)}',
        )
        texts = [
            get_artifact_text(event.artifact_update.artifact)
            for event in events
            if event.HasField('artifact_update')
        ]
        expect(texts == ['echo: hola'], f'the streamed artifacts are {texts}')

    async def get(self, binding: str) -> None:
        sent = self.sent.get(binding)
        expect(sent is not None, 'check 1 left no task')
        client = self.client(self.echo_card, binding, streaming=False)
        task = await client.get_task(GetTaskRequest(id=sent.id))
        expect(task.id == sent.id, f'GetTask returned task {task.id}')
        expect(task.context_id == sent.context_id, f'GetTask returned context {task.context_id}')
        expect(
            task.status.state == TaskState.TASK_STATE_COMPLETED,
            f'the task is {state_name(task.status.state)}',
        )
        expect(answer_of(task) == 'echo: hola', f'the answer is {answer_of(task)!r}')

    async def cancel(self, binding: str) -> None:
        client = self.client(self.slow_card, binding, streaming=False)
        request = SendMessageRequest(
            message=new_text_message('a long task', role=Role.ROLE_USER),
            configuration=SendMessageConfiguration(return_immediately=True),
        )
        request.message.message_id = f'test-resubscribe-message-id-{uuid.uuid4()}'
        responses = [response async for response in client.send_message(request)]
        expect(len(responses) == 1 and responses[0].HasField('task'), f'no task: {responses}')
        started = responses[0].task
        expect(
            started.status.state in (TaskState.TASK_STATE_SUBMITTED, TaskState.TASK_STATE_WORKING),
            f'the long task is {state_name(started.status.state)}',
        )
        canceled = await client.cancel_task(CancelTaskRequest(id=started.id))
        expect(canceled.id == started.id, f'CancelTask returned task {canceled.id}')
        expect(
            canceled.status.state == TaskState.TASK_STATE_CANCELED,
            f'CancelTask left the task {state_name(canceled.status.state)}',
        )
        task = await client.get_task(GetTaskRequest(id=started.id))
        expect(
            task.status.state == TaskState.TASK_STATE_CANCELED,
            f'the task is {state_name(task.status.state)} after CancelTask',
        )


async def run_checks(gateway: str, echo_id: str, slow_id: str) -> int:
    async with httpx.AsyncClient(timeout=HTTP_TIMEOUT_S) as http:
        cards = []
        for provider in (echo_id, slow_id):
            card = await A2ACardResolver(http, f'{gateway}/peers/{provider}').get_agent_card()
            bindings = {interface.protocol_binding: interface.url for interface in card.supported_interfaces}
            expected = {
                'JSONRPC': f'{gateway}/peers/{provider}/jsonrpc',
                'HTTP+JSON': f'{gateway}/peers/{provider}/rest',
            }
            if bindings != expected:
                print(f'FAIL: the card from the gateway declares {bindings}, not {expected}')
                return 1
            cards.append(card)
        print(f'card resolved from the gateway: {cards[0].name!r}, interfaces {", ".join(BINDINGS)}')

        checks = Checks(http, *cards)
        passed = 0
        for number, (title, check) in enumerate(
            [
                ('SendMessage answers "echo: hola"', checks.send),
                ('SendStreamingMessage ends TASK_STATE_COMPLETED', checks.stream),
                ('GetTask returns the completed task', checks.get),
                ('CancelTask leaves a long task TASK_STATE_CANCELED', checks.cancel),
            ],
            start=1,
        ):
            for binding in BINDINGS:
                try:
                    await check(binding)
                except CheckFailed as failed:
                    print(f'FAIL: check {number} ({title}) over {binding}: {failed}')
                    return 1
            print(f'check {number} passed: {title} ({", ".join(BINDINGS)})')
            passed += 1
        print(f'{passed} checks passed')
        return 0


def main() -> int:
    pals = pals_binary()
    logs = ROOT / 'target' / 'interop-run' / time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())
    logs.mkdir(parents=True, exist_ok=True)
    with ExitStack() as stack:
        daemons = Daemons(stack, pals, logs)
        daemons.start('echo', 'echo')
        daemons.start('slow', 'tck', env={'TCK_STREAMING_TIMEOUT': SLOW_TIMEOUT_S})
        daemons.start('requester', 'echo')
        echo_id = daemons.connect('requester', 'echo')
        slow_id = daemons.connect('requester', 'slow')
        gateway = f'http://127.0.0.1:{daemons.gateways["requester"]}'
        print(f'a2a-sdk against the gateway {gateway} (daemon logs in {logs})')
        return asyncio.run(run_checks(gateway, echo_id, slow_id))


if __name__ == '__main__':
    sys.exit(main())
