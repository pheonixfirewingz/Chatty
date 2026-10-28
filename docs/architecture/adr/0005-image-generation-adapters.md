# ADR 0005: Image generation adapters

Status: accepted
Recorded: 2026-10-27

## Context

Users want generated character portraits and illustrations in chats. Image servers
differ: ComfyUI queues workflow graphs and serves files, while other servers return
base64 data from one synchronous call. Text inference and image generation usually
run on different servers and models. The broker must keep provider schemas out of
its systems, as ADR 0003 requires for text inference.

## Decision

Add an optional image capability to adapter ABI 1. It uses `Image`, `ImageStatus`,
and `ImageCancel` requests, `ResponseKind::Image`, and `ImageJob` results.
Guests describe HTTP plans and decode replies. The host owns polling, deadlines,
cancellation, image downloads, and size bounds. Queue-based and synchronous servers
fit the same contract through `Pending` and `Ready` results.

Plugin manifests may declare `"kind": "image"`. Image plugins have their own broker
settings: enabled flag, URL, plugin, model, size, steps, guidance, and negative prompt.
They are never selectable as text runtimes. They load per operation and are not
retained, because jobs are rare and long-running. Bundle a ComfyUI plugin that runs
a single-checkpoint workflow.

The broker validates prompts and settings, supplies seeds, and limits concurrent
jobs. It re-encodes results as metadata-free PNG. Portrait generation returns an
unsaved image for the editor. Scene images are persisted as `image` messages, with
encrypted bytes in a separate table. Clients fetch those bytes on demand.

## Consequences

- No ABI version change; older guests reject image operations explicitly.
- Migration 0014 adds image settings and `message_images`; protocol version becomes 20.
- Job state is polled only while a request is active. There is no idle polling and
  no WebSocket client in the host.
- ComfyUI split-model layouts and custom workflows are not supported by the bundled
  plugin; a third-party image plugin can provide them without broker changes.
- Only PNG output is accepted, matching the existing image decoder features.
- When both services share a host, the broker admits one model kind at a time and
  unloads the other before switching (`ImageUnload`, or Ollama unload). This trades
  switch latency for staying within one GPU's memory. Hosts are compared by name,
  so different names for one machine are not detected.
