//! Caches, memory budgets and the chunk engine. The engine reads, decodes and caches chunks,
//! and makes the display tiles for the renderer.
mod engine;
pub mod pixels;

pub use engine::*;
pub use pixels::{Enc, Part, Pixels};

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

/// Least recently used cache with a byte budget.
pub struct Lru<K, V> {
    map: HashMap<K, (V, usize, u64)>,
    order: BTreeMap<u64, K>,
    tick: u64,
    pub bytes: usize,
    pub cap: usize,
}

impl<K: Hash + Eq + Clone, V> Lru<K, V> {
    pub fn new(cap: usize) -> Self {
        Lru { map: HashMap::new(), order: BTreeMap::new(), tick: 0, bytes: 0, cap }
    }

    pub fn get(&mut self, k: &K) -> Option<&V> {
        let e = self.map.get_mut(k)?;
        self.order.remove(&e.2);
        self.tick += 1;
        e.2 = self.tick;
        self.order.insert(self.tick, k.clone());
        Some(&e.0)
    }

    /// Insert a value of `size` bytes. Then remove the oldest values until the cache is in its budget.
    pub fn insert(&mut self, k: K, v: V, size: usize) {
        if let Some(old) = self.map.remove(&k) {
            self.order.remove(&old.2);
            self.bytes -= old.1;
        }
        self.tick += 1;
        self.order.insert(self.tick, k.clone());
        self.map.insert(k, (v, size, self.tick));
        self.bytes += size;
        while self.bytes > self.cap {
            let Some((_, k)) = self.order.pop_first() else { break };
            if let Some(e) = self.map.remove(&k) {
                self.bytes -= e.1;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Total system RAM in bytes.
pub fn system_ram() -> usize {
    // ponytail: Linux only. Other systems use 16 GB. Add sysctl and GlobalMemoryStatusEx when a user needs it.
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("MemTotal:"))?.split_whitespace().nth(1)?.parse::<usize>().ok())
        .map_or(16 << 30, |kb| kb << 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_oldest() {
        let mut c = Lru::new(10);
        c.insert(1, 'a', 4);
        c.insert(2, 'b', 4);
        c.get(&1);
        c.insert(3, 'c', 4);
        assert!(c.get(&2).is_none() && c.get(&1).is_some() && c.get(&3).is_some());
        assert_eq!(c.bytes, 8);
    }
}
