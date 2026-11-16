# Borradores de issues para el SDK oficial de Rust

Defectos del SDK fijado (`a2a-lf =0.4.2`, `a2a-server-lf =0.5.3`, con `a2a-pb 0.3.2`) que hacían fallar requisitos MUST del TCK de A2A (`a2aproject/a2a-tck` en `104a7b4ae0d8`), o que incumplen requisitos MUST de la especificación aunque el TCK no lo detecte (el 5). PALS los rodea en sus propias capas; estos textos son para abrirlos aguas arriba en `a2aproject/a2a-rs`. **No se han enviado:** abrirlos es una acción externa que decide el responsable. Van en inglés, como el repositorio de destino.

Cada borrador se puede reproducir con el servidor de ejemplo del SDK; los números de línea son los de las versiones citadas.

---

## 1. A `SendMessage` request without `message` is accepted and creates a task

**Crates:** `a2a-pb 0.3.2` (`pbconv::from_proto_send_message_request`), `a2a-server-lf 0.5.3` (`jsonrpc.rs`, `rest.rs`)

**TCK:** CORE-ERR-002 (MUST, §3.3.2: "Servers MUST validate all input parameters before processing"), both JSON-RPC and HTTP+JSON.

**What happens.** `from_proto_send_message_request` maps an absent `message` to `Message::new(Role::User, vec![])`, a fresh message with a newly generated `messageId` and no parts. Neither binding validates the REQUIRED fields of `Message` (`message_id`, `role`, `parts` in `a2a.proto`), so the request runs and the agent creates a task for a message nobody sent.

**Reproduce.**

```
POST /  (JSON-RPC)   {"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{}}
POST /message:send   {}
```

Both answer with a task (`200`), whose history holds a message with a server-generated `messageId` and no parts.

**Expected.** `InvalidParamsError`: JSON-RPC `-32602`; HTTP+JSON `400` with a `google.rpc.Status` whose `ErrorInfo.reason` is `INVALID_PARAMS`.

**Suggested fix.** Treat an absent `message` as an error in `from_proto_send_message_request` (or validate it in the handler), and check the REQUIRED fields of `Message` (non-empty `message_id`, a role other than unspecified, at least one part) before dispatch.

---

## 2. The HTTP+JSON binding does not check `A2A-Version`

**Crate:** `a2a-server-lf 0.5.3` (`rest::rest_router`; compare `jsonrpc::check_a2a_version`)

**TCK:** VER-SERVER-002 (MUST, §3.6.2) and the version case of HTTP_JSON-STATUS-001 (MUST, §5.4), on HTTP+JSON. JSON-RPC passes both.

**What happens.** The JSON-RPC router refuses a request whose `A2A-Version` names an unsupported major version, an absent header counting as `0.3` (`check_a2a_version`). The HTTP+JSON router has no such check: `A2A-Version: 99.0`, or no header at all, is served normally.

**Reproduce.** `GET /tasks/{id}` with `A2A-Version: 99.0` answers `404 TASK_NOT_FOUND` (or `200` for an existing task) instead of `400 VERSION_NOT_SUPPORTED`.

**Expected.** `400` with a `google.rpc.Status` (`status: FAILED_PRECONDITION`) whose `ErrorInfo.reason` is `VERSION_NOT_SUPPORTED`, for every HTTP+JSON route, with the same rules as `check_a2a_version`.

**Suggested fix.** Apply `check_a2a_version` to the service parameters of every REST handler (or as a layer on `rest_router`).

---

## 3. HTTP+JSON errors are sent as `application/problem+json`

**Crate:** `a2a-server-lf 0.5.3` (`rest::rest_error_response`, `rest::with_content_type`)

**TCK:** HTTP_JSON-ERR-001 (MUST, §11.6) requires a JSON content type containing `application/json`; HTTP_JSON-SVC-001 (SHOULD, §11.1) prefers `application/a2a+json`.

**What happens.** Error responses carry `Content-Type: application/problem+json`, the media type of RFC 9457 problem details, while their body is the `google.rpc.Status` JSON of A2A §11.6 (`{"error": {"code", "status", "message", "details"}}`), not a problem-details document.

**Expected.** A media type that describes the body: §11.6's example uses `application/a2a+json`; the TCK's MUST check accepts `application/json`.

**Suggested fix.** Send errors with the same media type as successful responses, or `application/json`. Note for the TCK: HTTP_JSON-ERR-001 and HTTP_JSON-SVC-001 cannot both pass today, since `application/a2a+json` does not contain the substring `application/json`; that may deserve its own issue in `a2aproject/a2a-tck`.

---

## 4. `SubscribeToTask` answers `TaskNotFoundError` for an existing task waiting for input

**Crate:** `a2a-server-lf 0.5.3` (`handler::DefaultRequestHandler::subscribe_to_task`)

**TCK:** STREAM-SUB-001, STREAM-SUB-002, STREAM-ORDER-002, STREAM-ORDER-003, STREAM-ORDER-004 (MUST, §3.1.6). The TCK skips them, so they show as not tested rather than failed.

**What happens.** `subscribe_to_task` only follows a task with a live execution. When the registry has none, it checks the store and refuses a terminal task with `UnsupportedOperationError`, but answers `TaskNotFoundError` for a task that exists and is not terminal, such as one in `TASK_STATE_INPUT_REQUIRED` whose execution ended when it asked for input.

**Reproduce.** Make an executor emit `INPUT_REQUIRED` and end; then `SubscribeToTask` with that task's id: `-32001 task not found`, while `GetTask` returns the task.

**Expected.** A stream whose first event is the `Task` as it is now, followed by its updates (including those of the execution a later message starts) until the task reaches a terminal state.

**Suggested fix.** On a registry miss for a non-terminal stored task, start a subscription that emits the stored task and attaches to the next execution of that task (the execution manager could keep a per-task broadcast channel that outlives a single execution).

---

## 5. `InMemoryTaskStore::list` sorts by task id, pages by offset and always returns artifacts

**Crate:** `a2a-server-lf 0.5.3` (`task_store::inmemory::InMemoryTaskStore::list`, `handler::DefaultRequestHandler::list_tasks`)

**TCK:** CORE-LIST-002, CORE-LIST-003 and CORE-LIST-005 (MUST, §3.1.4). The TCK lists an empty context, so it checks only the shape of the response and these pass anyway.

**What happens.** `list` sorts the matching tasks by `id` ("for deterministic output"), returns `pageToken` as a decimal offset into that order, and never looks at `include_artifacts`: each task comes with its artifacts. Neither `list_tasks` in the handler nor the REST and JSON-RPC routers drop them afterwards.

**Reproduce.** Create two tasks, the second with an id that sorts first, then `ListTasks {}`: the second task is listed last although it was updated last, and both carry `artifacts`. Update a task between two `pageSize: 1` requests: offsets shift, and a task is listed twice or skipped.

**Expected.** §3.1.4: "Implementations MUST return tasks sorted by their status timestamp time in descending order (most recently updated tasks first)"; "MUST use cursor-based pagination"; "When `includeArtifacts` is false (the default), the artifacts field MUST be omitted entirely from each Task object in the response."

**Suggested fix.** Sort by `status.timestamp` descending with the task id as tie-break, encode the last task's (timestamp, id) as an opaque cursor in `nextPageToken` and resume strictly after it, and clear `artifacts` unless `include_artifacts` is true (in the store, or in `list_tasks` for every store).
