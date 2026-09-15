# ADR 0002: Hybrid public PKI and private certificate enrollment

## Status

Accepted

## Context

Chatty brokers are used both with public DNS names and on private networks. Requiring users to copy
a private CA file to every client is cumbersome, while silently trusting a certificate downloaded
from an unauthenticated first connection would permit a man-in-the-middle attack.

## Decision

The GUI uses normal public PKI validation for publicly certified brokers. Existing explicit and
per-server CA configuration remains supported and takes priority.

When no configured trust material exists and public validation fails, the GUI completes a
credential-free TLS discovery handshake and calculates the last certificate in the presented
chain's SHA-256 fingerprint. The user must confirm it through a separate trusted channel. When the
chain includes a private CA, that CA becomes a per-server trust anchor and normal hostname, expiry,
and chain validation applies. A leaf-only legacy chain falls back to an exact-certificate pin. TLS
CertificateVerify signatures are still checked during discovery and pinned use.

An unexpected CA or legacy leaf change is rejected. CA replacement requires deleting the old trust
anchor and confirming the new fingerprint.

## Consequences

- Public deployments require no Chatty-specific CA distribution and renew normally.
- Private deployments require one fingerprint comparison but no file transfer.
- First-use security depends on the user comparing the fingerprint through a trusted channel.
- Private leaf-certificate renewal works normally when the broker supplies its private CA chain;
  legacy leaf-only pins require explicit re-enrollment.
- The broker protocol and account authentication messages remain unchanged.
