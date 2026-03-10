use {mimisbrunnr_types::ObjectId, roaring::RoaringBitmap};

/// Storage variant for a tag's membership data.
///
/// Most tags are simple bitmaps. Ordered collections (playlists, albums)
/// carry additional sequence data because the ontology says they should.
#[derive(Debug, Clone)]
pub enum TagStore {
    /// Simple membership bitmap.
    Simple(RoaringBitmap),
    /// Ordered collection: bitmap for membership + sequence for ordering.
    Ordered {
        members: RoaringBitmap,
        sequence: Vec<ObjectId>,
    },
    /// Ranked collection: bitmap + scored entries.
    Ranked {
        members: RoaringBitmap,
        ranked: Vec<(ObjectId, f32)>,
    },
}

impl TagStore {
    pub fn new_simple() -> Self {
        Self::Simple(RoaringBitmap::new())
    }

    pub fn new_ordered() -> Self {
        Self::Ordered {
            members: RoaringBitmap::new(),
            sequence: Vec::new(),
        }
    }

    pub fn new_ranked() -> Self {
        Self::Ranked {
            members: RoaringBitmap::new(),
            ranked: Vec::new(),
        }
    }

    /// Get the membership bitmap (all variants have one).
    pub fn bitmap(&self) -> &RoaringBitmap {
        match self {
            Self::Simple(bm) => bm,
            Self::Ordered { members, .. } => members,
            Self::Ranked { members, .. } => members,
        }
    }

    /// Get a mutable reference to the membership bitmap.
    pub fn bitmap_mut(&mut self) -> &mut RoaringBitmap {
        match self {
            Self::Simple(bm) => bm,
            Self::Ordered { members, .. } => members,
            Self::Ranked { members, .. } => members,
        }
    }

    /// Add an object to this tag store.
    pub fn insert(&mut self, obj_local: u32) {
        self.bitmap_mut().insert(obj_local);
    }

    /// Remove an object from this tag store.
    pub fn remove(&mut self, obj_local: u32) -> bool {
        let removed = self.bitmap_mut().remove(obj_local);
        // Also remove from sequence/ranked if applicable
        match self {
            Self::Ordered { sequence, .. } => {
                let oid = ObjectId::from_raw(obj_local as u64);
                sequence.retain(|o| *o != oid);
            }
            Self::Ranked { ranked, .. } => {
                let oid = ObjectId::from_raw(obj_local as u64);
                ranked.retain(|(o, _)| *o != oid);
            }
            Self::Simple(_) => {}
        }
        removed
    }

    /// Check membership.
    pub fn contains(&self, obj_local: u32) -> bool {
        self.bitmap().contains(obj_local)
    }

    /// Number of members.
    pub fn len(&self) -> u64 {
        self.bitmap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.bitmap().is_empty()
    }

    /// For ordered collections: append an object to the sequence.
    pub fn push_ordered(&mut self, oid: ObjectId) {
        if let Self::Ordered { members, sequence } = self {
            members.insert(oid.local().value() as u32);
            sequence.push(oid);
        }
    }

    /// For ordered collections: get the sequence.
    pub fn sequence(&self) -> Option<&[ObjectId]> {
        match self {
            Self::Ordered { sequence, .. } => Some(sequence),
            _ => None,
        }
    }

    /// For ranked collections: insert with score.
    pub fn insert_ranked(&mut self, oid: ObjectId, score: f32) {
        if let Self::Ranked { members, ranked } = self {
            members.insert(oid.local().value() as u32); // u32 cuts off local sequence high bits!
            ranked.push((oid, score));
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        }
    }

    /// For ranked collections: get ranked entries.
    pub fn ranked(&self) -> Option<&[(ObjectId, f32)]> {
        match self {
            Self::Ranked { ranked, .. } => Some(ranked),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, arbitrary_int::u48};

    #[test]
    fn simple_store() {
        let mut store = TagStore::new_simple();
        store.insert(1);
        store.insert(5);
        store.insert(42);

        assert!(store.contains(1));
        assert!(store.contains(5));
        assert!(!store.contains(2));
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn simple_remove() {
        let mut store = TagStore::new_simple();
        store.insert(1);
        store.insert(2);
        assert!(store.remove(1));
        assert!(!store.contains(1));
        assert!(store.contains(2));
        assert!(!store.remove(99)); // not present
    }

    #[test]
    fn ordered_store() {
        let mut store = TagStore::new_ordered();
        let oid1 = ObjectId::new(0, u48::from_u64(1));
        let oid2 = ObjectId::new(0, u48::from_u64(2));
        let oid3 = ObjectId::new(0, u48::from_u64(3));

        store.push_ordered(oid1);
        store.push_ordered(oid2);
        store.push_ordered(oid3);

        assert_eq!(store.len(), 3);
        assert!(store.contains(1));
        assert!(store.contains(2));
        assert!(store.contains(3));

        let seq = store.sequence().unwrap();
        assert_eq!(seq, &[oid1, oid2, oid3]);
    }

    #[test]
    fn ranked_store() {
        let mut store = TagStore::new_ranked();
        let oid1 = ObjectId::new(0, u48::from_u64(1));
        let oid2 = ObjectId::new(0, u48::from_u64(2));

        store.insert_ranked(oid1, 3.5);
        store.insert_ranked(oid2, 9.0);

        let ranked = store.ranked().unwrap();
        // Highest score first
        assert_eq!(ranked[0].0, oid2);
        assert_eq!(ranked[1].0, oid1);
    }

    #[test]
    fn bitmap_access() {
        let mut store = TagStore::new_simple();
        store.insert(10);
        store.insert(20);

        let bm = store.bitmap();
        assert!(bm.contains(10));
        assert!(bm.contains(20));
        assert_eq!(bm.len(), 2);
    }

    #[test]
    fn empty_store() {
        let store = TagStore::new_simple();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }
}
