//! In-memory sign-in throttle.
use super::*;

/// Failed sign-ins allowed per client within [`LOGIN_WINDOW`].
const LOGIN_FAILURES: u32 = 10;
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Bounds the throttle's memory. When full, the oldest window is dropped.
const LOGIN_TRACKED: usize = 4096;

/// Throttle key for a sign-in: the client's IPv4 address or IPv6 /64, which
/// one host typically controls whole. Keying by client rather than username
/// stops guessing without letting others lock a user out from elsewhere.
/// Falls back to the username when the address is unknown.
pub(crate) fn login_throttle_key(peer: Option<std::net::IpAddr>, username: &str) -> String {
    use std::net::IpAddr;
    match peer.map(|ip| ip.to_canonical()) {
        Some(IpAddr::V4(ip)) => ip.to_string(),
        Some(IpAddr::V6(ip)) => {
            let s = ip.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        None => format!("user:{username}"),
    }
}

/// Failed sign-in counts per throttle key, kept in memory only. Failures for
/// unknown usernames count too, so throttling reveals nothing about accounts.
#[derive(Default)]
pub(crate) struct LoginThrottle(pub(crate) std::sync::Mutex<HashMap<String, (u32, Instant)>>);

impl LoginThrottle {
    pub(crate) fn check(&self, username: &str, now: Instant) -> Result<()> {
        let failures = self.0.lock().unwrap();
        match failures.get(username) {
            Some((count, start))
                if *count >= LOGIN_FAILURES && now.duration_since(*start) < LOGIN_WINDOW =>
            {
                bail!("too many failed sign-ins; try again in 15 minutes")
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn failed(&self, username: &str, now: Instant) {
        let mut failures = self.0.lock().unwrap();
        if !failures.contains_key(username) && failures.len() >= LOGIN_TRACKED {
            failures.retain(|_, (_, start)| now.duration_since(*start) < LOGIN_WINDOW);
            if failures.len() >= LOGIN_TRACKED
                && let Some(oldest) = failures
                    .iter()
                    .min_by_key(|(_, (_, start))| *start)
                    .map(|(name, _)| name.clone())
            {
                failures.remove(&oldest);
            }
        }
        let entry = failures.entry(username.to_owned()).or_insert((0, now));
        if now.duration_since(entry.1) >= LOGIN_WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    pub(crate) fn succeeded(&self, username: &str) {
        self.0.lock().unwrap().remove(username);
    }
}

pub(crate) fn login_throttle() -> &'static LoginThrottle {
    static THROTTLE: std::sync::OnceLock<LoginThrottle> = std::sync::OnceLock::new();
    THROTTLE.get_or_init(LoginThrottle::default)
}
