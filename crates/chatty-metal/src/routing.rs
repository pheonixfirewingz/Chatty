//! Expert routing trace. Only 6 of 384 experts run per layer per token, so which ones repeat
//! decides cache hit rates, drive traffic, and whether read-ahead can pay for itself.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TierHits {
    pub vram: u64,
    pub ram: u64,
    pub drive: u64,
}

impl TierHits {
    pub fn total(self) -> u64 {
        self.vram + self.ram + self.drive
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    pub uses: u64,
    pub distinct: usize,
    /// Share of uses that went to the busiest tenth of the experts seen.
    pub top_tenth_share: f64,
    /// Share of a token's experts the previous token also used at that layer.
    pub repeat_share: f64,
}

/// `(token, layer, expert)` uses in routing order.
#[derive(Clone, Debug, Default)]
pub struct RouteTrace {
    tokens: u32,
    uses: Vec<(u32, u8, u16)>,
}

impl RouteTrace {
    /// Start a new token and return its index.
    pub fn next_token(&mut self) -> u32 {
        self.tokens += 1;
        self.tokens - 1
    }

    pub fn record(&mut self, token: u32, layer: usize, expert: usize) {
        self.uses.push((token, layer as u8, expert as u16));
    }

    /// Uses per (layer, expert), most used first.
    pub fn counts(&self) -> Vec<(u8, u16, u32)> {
        let mut counts: HashMap<(u8, u16), u32> = HashMap::new();
        for &(_, layer, expert) in &self.uses {
            *counts.entry((layer, expert)).or_default() += 1;
        }
        let mut sorted: Vec<(u8, u16, u32)> =
            counts.into_iter().map(|((l, e), c)| (l, e, c)).collect();
        sorted.sort_unstable_by_key(|entry| (std::cmp::Reverse(entry.2), entry.0, entry.1));
        sorted
    }

    pub fn tokens(&self) -> u32 {
        self.tokens
    }

    pub fn summary(&self) -> Summary {
        let mut counts: HashMap<(u8, u16), u64> = HashMap::new();
        let mut last_seen: HashMap<(u8, u16), u32> = HashMap::new();
        let mut repeats = 0u64;
        let mut eligible = 0u64;
        for &(token, layer, expert) in &self.uses {
            *counts.entry((layer, expert)).or_default() += 1;
            if token > 0 {
                eligible += 1;
                if last_seen.get(&(layer, expert)) == Some(&(token - 1)) {
                    repeats += 1;
                }
            }
            last_seen.insert((layer, expert), token);
        }
        let mut sorted: Vec<u64> = counts.values().copied().collect();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        let top = sorted.len().div_ceil(10);
        let uses = self.uses.len() as u64;
        Summary {
            uses,
            distinct: sorted.len(),
            top_tenth_share: ratio(sorted.iter().take(top).sum(), uses),
            repeat_share: ratio(repeats, eligible),
        }
    }

    /// Replay through exclusive LRU tiers, as the caches would serve it from cold.
    pub fn replay(&self, vram_slots: usize, ram_slots: usize) -> TierHits {
        let mut vram = Lru::new(vram_slots);
        let mut ram = Lru::new(ram_slots);
        let mut hits = TierHits::default();
        for &(_, layer, expert) in &self.uses {
            let key = (layer, expert);
            if vram.touch(key) {
                hits.vram += 1;
                continue;
            }
            if ram.remove(key) {
                hits.ram += 1;
            } else {
                hits.drive += 1;
            }
            if let Some(evicted) = vram.insert(key) {
                ram.insert(evicted);
            }
        }
        hits
    }
}

fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

type Key = (u8, u16);

/// Tick-stamped LRU. Eviction scans, which is fine for an offline replay of a bounded trace.
struct Lru {
    capacity: usize,
    tick: u64,
    entries: HashMap<Key, u64>,
}

impl Lru {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            tick: 0,
            entries: HashMap::with_capacity(capacity),
        }
    }

    fn touch(&mut self, key: Key) -> bool {
        self.tick += 1;
        match self.entries.get_mut(&key) {
            Some(stamp) => {
                *stamp = self.tick;
                true
            }
            None => false,
        }
    }

    fn remove(&mut self, key: Key) -> bool {
        self.entries.remove(&key).is_some()
    }

    /// Insert and return the evicted entry, if the tier was full.
    fn insert(&mut self, key: Key) -> Option<Key> {
        if self.capacity == 0 {
            return Some(key);
        }
        let mut evicted = None;
        if self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, stamp)| **stamp)
                .map(|(k, _)| *k);
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
                evicted = Some(oldest);
            }
        }
        self.tick += 1;
        self.entries.insert(key, self.tick);
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(tokens: &[&[u16]]) -> RouteTrace {
        let mut t = RouteTrace::default();
        for experts in tokens {
            let token = t.next_token();
            for &expert in *experts {
                t.record(token, 0, usize::from(expert));
            }
        }
        t
    }

    #[test]
    fn repeat_share_counts_only_the_previous_token() {
        let s = trace(&[&[1, 2], &[2, 3], &[1, 3]]).summary();
        assert_eq!(s.uses, 6);
        assert_eq!(s.distinct, 3);
        // Of four eligible uses, 2 and 3 repeat from the token before; 1 skipped one token.
        assert!((s.repeat_share - 0.5).abs() < 1e-9);
    }

    #[test]
    fn top_tenth_share_finds_skew() {
        let many: Vec<u16> = (0..10).collect();
        let s = trace(&[&many, &[0], &[0], &[0]]).summary();
        assert!((s.top_tenth_share - 4.0 / 13.0).abs() < 1e-9);
    }

    #[test]
    fn replay_keeps_tiers_exclusive() {
        // 1 misses, 2 misses and pushes 1 to RAM, 1 returns from RAM and pushes 2 down.
        let hits = trace(&[&[1], &[2], &[1], &[1]]).replay(1, 1);
        assert_eq!(
            hits,
            TierHits {
                vram: 1,
                ram: 1,
                drive: 2
            }
        );
    }

    #[test]
    fn replay_with_no_cache_always_reads_the_drive() {
        let hits = trace(&[&[1], &[1]]).replay(0, 0);
        assert_eq!(hits.drive, 2);
        assert_eq!(hits.total(), 2);
    }
}
