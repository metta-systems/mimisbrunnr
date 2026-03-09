use std::collections::HashMap;

use {
    mimisbrunnr_types::{TagId, Value},
    roaring::RoaringBitmap,
};

/// Key-Value equality index.
///
/// Treats `(tag_id, value)` as a compound key. Stored with composite keys
/// `[tag_id: 4B][value_hash: 8B]`, enabling prefix scans over all values
/// for a given key.
#[derive(Clone)]
pub struct KvIndex {
    /// Maps (tag_id, value_hash) → bitmap of object locals.
    entries: HashMap<(TagId, u64), RoaringBitmap>,
}

impl KvIndex {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Insert an object into the KV index for a given key-value pair.
    pub fn insert(&mut self, key: TagId, value: &Value, obj_local: u32) {
        let hash = hash_value(value);
        self.entries
            .entry((key, hash))
            .or_default()
            .insert(obj_local);
    }

    /// Remove an object from the KV index for a given key-value pair.
    pub fn remove(&mut self, key: TagId, value: &Value, obj_local: u32) {
        let hash = hash_value(value);
        if let Some(bm) = self.entries.get_mut(&(key, hash)) {
            bm.remove(obj_local);
            if bm.is_empty() {
                self.entries.remove(&(key, hash));
            }
        }
    }

    /// Lookup: find all objects where `key = value`.
    pub fn lookup_eq(&self, key: TagId, value: &Value) -> RoaringBitmap {
        let hash = hash_value(value);
        self.entries.get(&(key, hash)).cloned().unwrap_or_default()
    }

    /// Get all distinct value hashes for a given key (for faceted exploration).
    pub fn values_for_key(&self, key: TagId) -> Vec<u64> {
        self.entries
            .keys()
            .filter(|(k, _)| *k == key)
            .map(|(_, h)| *h)
            .collect()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

impl Default for KvIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Simple hash of a Value for use as a lookup key.
/// Uses the built-in hasher for practical correctness.
fn hash_value(value: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match value {
        Value::Text(s) => {
            0u8.hash(&mut hasher);
            s.hash(&mut hasher);
        }
        Value::Int(v) => {
            1u8.hash(&mut hasher);
            v.hash(&mut hasher);
        }
        Value::Float(v) => {
            2u8.hash(&mut hasher);
            v.to_bits().hash(&mut hasher);
        }
        Value::Timestamp(v) => {
            3u8.hash(&mut hasher);
            v.hash(&mut hasher);
        }
        Value::Blob(v) => {
            4u8.hash(&mut hasher);
            v.hash(&mut hasher);
        }
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn insert_and_lookup() {
        let mut idx = KvIndex::new();
        let artist = tag(1);
        let val = Value::Text("Aphex Twin".into());

        idx.insert(artist, &val, 10);
        idx.insert(artist, &val, 20);

        let result = idx.lookup_eq(artist, &val);
        assert_eq!(result.len(), 2);
        assert!(result.contains(10));
        assert!(result.contains(20));
    }

    #[test]
    fn different_values_separate() {
        let mut idx = KvIndex::new();
        let artist = tag(1);

        idx.insert(artist, &Value::Text("Aphex Twin".into()), 10);
        idx.insert(artist, &Value::Text("Boards of Canada".into()), 20);

        let r1 = idx.lookup_eq(artist, &Value::Text("Aphex Twin".into()));
        assert_eq!(r1.len(), 1);
        assert!(r1.contains(10));

        let r2 = idx.lookup_eq(artist, &Value::Text("Boards of Canada".into()));
        assert_eq!(r2.len(), 1);
        assert!(r2.contains(20));
    }

    #[test]
    fn remove_entry() {
        let mut idx = KvIndex::new();
        let key = tag(1);
        let val = Value::Int(2024);

        idx.insert(key, &val, 5);
        idx.insert(key, &val, 6);
        idx.remove(key, &val, 5);

        let result = idx.lookup_eq(key, &val);
        assert_eq!(result.len(), 1);
        assert!(result.contains(6));
    }

    #[test]
    fn lookup_nonexistent() {
        let idx = KvIndex::new();
        let result = idx.lookup_eq(tag(1), &Value::Int(999));
        assert!(result.is_empty());
    }

    #[test]
    fn int_values() {
        let mut idx = KvIndex::new();
        let year = tag(10);

        idx.insert(year, &Value::Int(2024), 1);
        idx.insert(year, &Value::Int(2024), 2);
        idx.insert(year, &Value::Int(2023), 3);

        assert_eq!(idx.lookup_eq(year, &Value::Int(2024)).len(), 2);
        assert_eq!(idx.lookup_eq(year, &Value::Int(2023)).len(), 1);
    }

    #[test]
    fn values_for_key() {
        let mut idx = KvIndex::new();
        let key = tag(1);

        idx.insert(key, &Value::Text("a".into()), 1);
        idx.insert(key, &Value::Text("b".into()), 2);
        idx.insert(key, &Value::Text("c".into()), 3);

        let vals = idx.values_for_key(key);
        assert_eq!(vals.len(), 3);
    }
}
