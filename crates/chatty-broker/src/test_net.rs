//! Loopback listeners for mock servers in tests.

/// Bind a mock server on an ephemeral loopback port.
///
/// Linux routes all of 127.0.0.0/8 to loopback. Binding 127.0.0.2 keeps mocks
/// away from development tools that probe every new `localhost` listener; their
/// stray requests would otherwise reach mocks that expect only broker traffic.
pub(crate) async fn mock_listener() -> tokio::net::TcpListener {
    #[cfg(target_os = "linux")]
    const ADDRESS: &str = "127.0.0.2:0";
    #[cfg(not(target_os = "linux"))]
    const ADDRESS: &str = "127.0.0.1:0";
    tokio::net::TcpListener::bind(ADDRESS).await.unwrap()
}
