//! Broker address parsing.

use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionTarget {
    pub broker: String,
    pub server_name: String,
}
pub(super) fn broker_host(value: &str) -> Option<String> {
    let value = value.trim();
    value
        .parse::<IpAddr>()
        .map(|ip| ip.to_string())
        .ok()
        .or_else(|| {
            value
                .parse::<SocketAddr>()
                .map(|address| address.ip().to_string())
                .ok()
        })
        .or_else(|| {
            let host = match value.rsplit_once(':') {
                Some((host, "7443")) if !host.contains(':') => host,
                Some(_) => return None,
                None => value,
            };
            let valid = !host.is_empty()
                && host.len() <= 253
                && host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .as_bytes()
                            .last()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                });
            valid.then(|| host.to_ascii_lowercase())
        })
}
pub(crate) fn connection_target(value: &str) -> Option<ConnectionTarget> {
    let host = broker_host(value)?;
    let broker = host.parse::<IpAddr>().map_or_else(
        |_| format!("{host}:7443"),
        |ip| SocketAddr::new(ip, 7443).to_string(),
    );
    Some(ConnectionTarget {
        broker,
        server_name: host,
    })
}
