//! Keeps text and image models from occupying one shared GPU host at once.
//!
//! When the inference and image services resolve to the same host, every
//! model-backed operation holds a [`WorkloadGuard`]. Operations of one kind run
//! concurrently; switching kinds waits for active work to finish, then unloads
//! the other kind's models. A waiting kind blocks new arrivals of the loaded
//! kind, so neither side can starve the other.
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Workload {
    Text,
    Image,
}
impl Workload {
    fn index(self) -> usize {
        self as usize
    }
    fn other(self) -> Self {
        match self {
            Self::Text => Self::Image,
            Self::Image => Self::Text,
        }
    }
}

#[derive(Default)]
struct GateState {
    /// Kind whose models may be resident. `None` until the first operation.
    loaded: Option<Workload>,
    active: usize,
    waiting: [usize; 2],
    /// The first holder of a new kind is unloading the other kind.
    switching: bool,
}

#[derive(Default)]
pub(super) struct WorkloadGate {
    state: Mutex<GateState>,
    changed: Notify,
}

/// Decrements the waiting count if an admission future is dropped while queued.
struct Waiting<'a> {
    gate: &'a WorkloadGate,
    kind: Workload,
    counted: bool,
}
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if self.counted {
            self.gate.lock().waiting[self.kind.index()] -= 1;
            self.gate.changed.notify_waiters();
        }
    }
}

impl WorkloadGate {
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        // State updates cannot panic midway, so a poisoned lock is still consistent.
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Wait for admission. `true` means the caller must unload the other kind,
    /// then call [`WorkloadGate::switched`].
    pub async fn admit(self: &Arc<Self>, kind: Workload) -> (WorkloadGuard, bool) {
        let mut waiting = Waiting {
            gate: self,
            kind,
            counted: false,
        };
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.lock();
                let other_waiting = state.waiting[kind.other().index()] > 0;
                let same = state.loaded == Some(kind);
                let admit = !state.switching
                    && if state.active == 0 {
                        !(same && other_waiting)
                    } else {
                        same && !other_waiting
                    };
                if admit {
                    if waiting.counted {
                        state.waiting[kind.index()] -= 1;
                        waiting.counted = false;
                    }
                    state.loaded = Some(kind);
                    state.active += 1;
                    state.switching = !same;
                    return (WorkloadGuard(Some(self.clone())), !same);
                }
                if !waiting.counted {
                    state.waiting[kind.index()] += 1;
                    waiting.counted = true;
                }
            }
            notified.await;
        }
    }

    pub fn switched(&self) {
        self.lock().switching = false;
        self.changed.notify_waiters();
    }
}

/// Admission for one model-backed operation; `None` when no host is shared.
#[must_use]
pub(super) struct WorkloadGuard(Option<Arc<WorkloadGate>>);
impl WorkloadGuard {
    pub fn unshared() -> Self {
        Self(None)
    }
}
impl Drop for WorkloadGuard {
    fn drop(&mut self) {
        if let Some(gate) = &self.0 {
            let mut state = gate.lock();
            state.active -= 1;
            // A holder dropped mid-switch leaves the other kind possibly loaded;
            // forget the kind so the next holder unloads again.
            if state.switching {
                state.switching = false;
                state.loaded = None;
            }
            drop(state);
            gate.changed.notify_waiters();
        }
    }
}

/// Compare hosts, ignoring scheme and port. Loopback names are equivalent.
pub(super) fn same_host(a: &str, b: &str) -> bool {
    fn host(url: &str) -> Option<String> {
        let host = reqwest::Url::parse(url)
            .ok()?
            .host_str()?
            .to_ascii_lowercase();
        Some(match host.as_str() {
            "localhost" | "127.0.0.1" | "[::1]" | "::1" => "loopback".into(),
            _ => host,
        })
    }
    matches!((host(a), host(b)), (Some(a), Some(b)) if a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn pending<T>(future: impl std::future::Future<Output = T>) -> bool {
        tokio::time::timeout(Duration::from_millis(50), future)
            .await
            .is_err()
    }

    #[tokio::test]
    async fn kinds_alternate_without_starvation() {
        let gate = Arc::new(WorkloadGate::default());
        let (text, switch) = gate.admit(Workload::Text).await;
        assert!(switch, "the first holder unloads the unknown other kind");
        gate.switched();
        let (second_text, switch) = gate.admit(Workload::Text).await;
        assert!(!switch, "same kind runs concurrently");

        let image_gate = gate.clone();
        let image = tokio::spawn(async move { image_gate.admit(Workload::Image).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            pending(gate.admit(Workload::Text)).await,
            "a waiting image blocks new text"
        );
        drop(text);
        assert!(!image.is_finished());
        drop(second_text);
        let (image, switch) = image.await.unwrap();
        assert!(switch, "image unloads text first");
        assert!(
            pending(gate.admit(Workload::Text)).await,
            "switching holds others"
        );
        gate.switched();
        drop(image);
        let (_text, switch) = gate.admit(Workload::Text).await;
        assert!(switch);
    }

    #[tokio::test]
    async fn cancelled_waiters_do_not_block_admission() {
        let gate = Arc::new(WorkloadGate::default());
        let (text, _) = gate.admit(Workload::Text).await;
        gate.switched();
        assert!(pending(gate.admit(Workload::Image)).await);
        let (_more_text, switch) = gate.admit(Workload::Text).await;
        assert!(!switch, "the dropped image waiter no longer has priority");
        drop(text);
    }

    #[test]
    fn hosts_match_by_name_without_port() {
        assert!(same_host(
            "http://192.168.0.97:11434/v1",
            "http://192.168.0.97:8188"
        ));
        assert!(same_host(
            "http://localhost:11434",
            "https://127.0.0.1:8188"
        ));
        assert!(!same_host(
            "http://192.168.0.97:11434",
            "http://192.168.0.98:8188"
        ));
        assert!(!same_host("not a url", "not a url"));
    }
}
