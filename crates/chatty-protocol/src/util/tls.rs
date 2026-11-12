//! TLS settings shared by the broker and its clients.

use rustls::crypto::{CryptoProvider, aws_lc_rs};
use std::sync::{Arc, OnceLock};

/// The aws-lc-rs provider restricted to the X25519MLKEM768 hybrid key
/// exchange for Chatty client-broker connections.
///
/// The hybrid stays secure while either X25519 or ML-KEM-768 holds, so
/// recorded traffic resists later decryption by a quantum computer. Peers
/// without ML-KEM support fail the handshake instead of downgrading.
/// Certificate signatures remain classical; they only matter to an attacker
/// active during the handshake.
pub fn post_quantum_provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| {
            Arc::new(CryptoProvider {
                kx_groups: vec![aws_lc_rs::kx_group::X25519MLKEM768],
                ..aws_lc_rs::default_provider()
            })
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::NamedGroup;

    #[test]
    fn provider_offers_only_the_hybrid_post_quantum_group() {
        let groups: Vec<NamedGroup> = post_quantum_provider()
            .kx_groups
            .iter()
            .map(|group| group.name())
            .collect();
        assert_eq!(groups, [NamedGroup::X25519MLKEM768]);
    }
}
