# Model connector architecture

## Invariants

- Every game chat and architect history persists one `ModelBinding` containing
  `connector_id` and `model_id`.
- The connector id is immutable for the lifetime of that history.
- The model may change only to another selectable model exposed by the same
  connector.
- Starting a new history is the only way to select another connector.
- Reset keeps the binding but starts fresh connector-owned cache identities.
- Child model clients (NPC, character and location generation) are created by
  the history's connector and inherit its current model.

## Responsibility boundaries

`gml-llm` is the provider-neutral connector SDK. It owns:

- the `Backend` runtime contract;
- connector descriptors, validated ids and the connector registry;
- the immutable `ModelBinding` value;
- shared opaque message and cache-identity helpers.

The application core owns:

- histories and persistence orchestration;
- canonical tool definitions and tool execution;
- world state, agent loops and transcript assembly;
- validation that a history keeps its original connector.

Each provider connector owns:

- authentication and credential file format;
- model discovery and selectable-model filtering;
- conversion of canonical messages and tools to its wire protocol;
- streaming response parsing and provider-specific hidden state;
- cache-key, conversation-state and model-change reset policy;
- legacy backend aliases that belong to that provider.

Provider implementations are isolated in:

- `gml-codex`;
- `gml-supergrok` (stable connector id `xai`);
- `gml-openai-compatible`;
- `gml-mock`.

`gml-app` is the composition root. It constructs and registers the available
connectors, selects only the default binding for new or legacy histories, and
passes the registry into persistence and the server. Core crates do not read a
process-wide provider choice.

## Authentication storage

Credentials are connector-owned JSON resources under the application config
directory:

```text
connectors/
  codex/auth.json
  xai/auth.json
```

No password prompt or OS keyring is required. Writes are atomic. OAuth refresh,
token rotation, logout and concurrent refresh coordination remain private to
the connector.

## Cache lifecycle

A history persists connector session/thread identities when the connector uses
them. A configured cache key is a namespace; the rotating thread id is always
part of the effective key. This prevents cache reuse across histories or model
changes while still allowing a stable application prefix.

Changing the model rotates all live connector cache identities and clears
provider conversation state. Reset constructs fresh clients with the same
binding. Connector-specific hidden response state is kept opaque inside stored
assistant messages and is never interpreted by the core.

## HTTP contract

The generic connector endpoints are:

- `GET /connectors`;
- `GET /connectors/{id}/models`;
- `GET /connectors/{id}/auth/status`;
- `POST /connectors/{id}/auth/start`;
- `POST /connectors/{id}/auth/logout`.

Compatibility Codex endpoints are adapters over the same registry and contain
no direct provider implementation.
