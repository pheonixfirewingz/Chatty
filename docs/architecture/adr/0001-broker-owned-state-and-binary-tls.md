# ADR 0001: Broker-owned state over a binary TLS protocol

Status: accepted  
Recorded: 2026-09-08

## Context

Chatty must support persistent multi-user roleplay over constrained networks without embedding a model runtime in the desktop application or broker. Account boundaries, shared state, reconnect behavior, and inference credentials/settings require one authoritative security boundary.

A conventional browser stack or stateless JSON API would add runtime weight and repeatedly transfer conversational state. Direct client access to SQLite or the inference service would distribute authorization and prompt rules across untrusted clients.

## Decision

Use a native Rust GUI connected to a Rust broker through TLS 1.3. The broker is authoritative for authentication, authorization, SQLite data, revisions, prompt assembly, and inference access.

Use a versioned JSON handshake only for protocol negotiation. Encode runtime requests, responses, deltas, errors, and streams with bincode 2 inside bounded frames. Compress streams, deltas, and larger payloads with zstd. Maintain a persistent connection while active, use bounded queues for backpressure, and resume from a durable revision log after reconnect.

Keep inference external through OpenAI-compatible HTTP streaming or Ollama's native API.

## Consequences

Benefits:

- One enforceable authorization and tenant-isolation boundary
- Low protocol overhead and compressed batched streaming
- Durable reconnect without polling or full-state transfer after every interruption
- Native client with no browser or Node.js runtime
- Independent model deployment and replacement

Costs:

- Binary type changes can require coordinated broker and GUI upgrades.
- Horizontal broker scaling needs new distributed coordination and a different data layer.
- Native Linux GUI packaging has more platform-specific dependencies than a browser client.
- Protocol inspection is less convenient than plain JSON and requires shared tooling/types.

## Guardrails

- Increment the handshake protocol number for incompatible wire changes.
- Preserve strict frame and decompression bounds.
- Perform every authorization decision in the broker.
- Persist a mutation before publishing its delta.
- Never add an insecure TLS mode to production paths.
- Keep model inference outside the broker and GUI processes.
