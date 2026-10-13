//! In-memory cache of loaded lists, so reopening a view shows its rows at once
//! while it refreshes (stale-while-revalidate).
//!
//! Keyed by profile, not account: two profiles on one account can have
//! different permissions, and one must never be shown what only the other can
//! read. Nothing is written to disk.

use std::collections::HashMap;
use std::time::Instant;

use fast_aws_core::Row;

/// Most lists kept; the least recently stored is dropped first.
const CAPACITY: usize = 64;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct CacheKey {
    pub profile: String,
    pub region: String,
    pub kind: &'static str,
    /// The parent's ID, for drilled-into lists.
    pub parent: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub rows: Vec<Row>,
    /// The page token the load paused at, if it did.
    pub more: Option<String>,
    pub stored_at: Instant,
    /// Store order, for eviction (Instants can tie).
    seq: u64,
}

#[derive(Default)]
pub struct Cache {
    entries: HashMap<CacheKey, Entry>,
    next_seq: u64,
}

impl Cache {
    pub fn get(&self, key: &CacheKey) -> Option<&Entry> {
        self.entries.get(key)
    }

    pub fn put(&mut self, key: CacheKey, rows: Vec<Row>, more: Option<String>) {
        if !self.entries.contains_key(&key) && self.entries.len() >= CAPACITY {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.seq)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.next_seq += 1;
        let entry = Entry {
            rows,
            more,
            stored_at: Instant::now(),
            seq: self.next_seq,
        };
        self.entries.insert(key, entry);
    }

    /// Every cached list for one profile and region, with its key.
    pub fn lists_for<'a>(
        &'a self,
        profile: &'a str,
        region: &'a str,
    ) -> impl Iterator<Item = (&'a CacheKey, &'a Entry)> + 'a {
        self.entries
            .iter()
            .filter(move |(k, _)| k.profile == profile && k.region == region)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(profile: &str, kind: &'static str) -> CacheKey {
        CacheKey {
            profile: profile.into(),
            region: "eu-west-1".into(),
            kind,
            parent: None,
        }
    }

    #[test]
    fn profiles_never_share_entries() {
        let mut cache = Cache::default();
        cache.put(
            key("qa-admin", "iam:role"),
            vec![Row::new("secret-role", vec![])],
            None,
        );
        assert!(cache.get(&key("qa-ro", "iam:role")).is_none());
        assert_eq!(
            cache.get(&key("qa-admin", "iam:role")).unwrap().rows.len(),
            1
        );
    }

    #[test]
    fn the_oldest_entry_goes_when_full() {
        let mut cache = Cache::default();
        let kinds: Vec<&'static str> = (0..=CAPACITY)
            .map(|i| &*Box::leak(format!("k{i}").into_boxed_str()))
            .collect();
        for k in &kinds {
            cache.put(key("p", k), Vec::new(), None);
        }
        assert_eq!(cache.len(), CAPACITY);
        assert!(cache.get(&key("p", kinds[0])).is_none());
        assert!(cache.get(&key("p", kinds[CAPACITY])).is_some());
    }
}
