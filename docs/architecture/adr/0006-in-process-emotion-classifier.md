# ADR 0006: In-process emotion classifier

Status: accepted
Recorded: 2026-10-28

## Context

Replies show a character portrait chosen by emotion. The text model used to pick
it after each reply, before the reply was persisted. Chat waited on a second model
call, and the choice competed with chat for the inference server. A dedicated
classifier, JusteLeo/emotion-text-classifier-LLM (Gemma 3 1B, about 690 MB GGUF),
returns a fixed set of emotions and is small enough to run on the broker host.

## Decision

Run the classifier inside the broker through llama.cpp (`llama-cpp-2`, CPU only).
This is the one exception to keeping model inference external. Administrators set
the GGUF path on the broker host; an empty path disables classification.

The broker persists each reply with the default portrait and ends the stream first.
A background task then classifies the reply text on a blocking thread and maps the
first matching emotion to a portrait label. A portrait label equal to the emotion
wins; otherwise a fixed table maps classifier emotions to the editor's 12 labels.
The task updates the message only when the owner still has it, the same variant is
selected, and its portrait is unchanged. It appends a `message` update delta and
publishes it to all of the owner's connections.

## Consequences

- Chat never waits on portrait selection and no longer spends inference tokens on it.
- The broker loads the model on first use and keeps it resident (about 1 GB RAM).
  Classifications run one at a time on half the CPU cores. At most eight replies
  wait; later replies keep the default portrait.
- llama.cpp is compiled from source, so broker builds need CMake and a C++ compiler.
- Classification sees only the reply, not earlier turns.
- Migration 0016 adds the setting; protocol version becomes 23.
