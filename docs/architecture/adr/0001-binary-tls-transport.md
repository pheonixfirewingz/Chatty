# ADR 0001: TLS 1.3 framed binary transport

Status: accepted

The mobile/rural-hosting constraint makes packet count and transferred bytes primary. The client/broker contract therefore uses persistent TLS 1.3, a 14-byte fixed header, bincode payloads, zstd compression, revision deltas, and batched text chunks. A bounded send queue supplies backpressure. This is less browser-compatible and less human-inspectable than HTTP/JSON, but it directly meets the native, low-bandwidth boundary and keeps JSON confined to the broker's llama.cpp adapter.

