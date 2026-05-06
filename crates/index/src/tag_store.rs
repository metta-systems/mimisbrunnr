//! `TagStore` enum (DESIGN §5.2) — Simple / Ordered / Ranked.
//!
//! All three variants carry a roaring membership bitmap. `Ordered` adds a
//! sequence vector (playlist-style ordering); `Ranked` adds a scored vector
//! (recommender-style ordering).

use {
    mimisbrunnr_types::ObjectId,
    roaring::RoaringBitmap,
    serde::{Deserialize, Serialize},
};

/// Storage variant for a tag's membership data.
#[derive(Debug, Clone)]
pub enum TagStore {
    /// Plain membership bitmap — the common case (DESIGN §5.1).
    Simple(RoaringBitmap),
    /// Ordered collection: membership + sequence (DESIGN §5.2). The sequence
    /// preserves insertion / curator order (playlist tracks, album positions).
    Ordered {
        members: RoaringBitmap,
        sequence: Vec<ObjectId>,
    },
    /// Ranked collection: membership + scored entries (DESIGN §5.2). Score is
    /// `i64` so that the in-memory mirror is `Ord`-able by score without the
    /// `f32` `NaN` foot-gun. Callers that want decimal scores convert via
    /// fixed-point.
    Ranked {
        members: RoaringBitmap,
        ranked: Vec<(ObjectId, i64)>,
    },
}

impl TagStore {
    /// Empty `Simple` store.
    pub fn new_simple() -> Self {
        Self::Simple(RoaringBitmap::new())
    }

    /// Empty `Ordered` store.
    pub fn new_ordered() -> Self {
        Self::Ordered {
            members: RoaringBitmap::new(),
            sequence: Vec::new(),
        }
    }

    /// Empty `Ranked` store.
    pub fn new_ranked() -> Self {
        Self::Ranked {
            members: RoaringBitmap::new(),
            ranked: Vec::new(),
        }
    }

    /// Borrow the membership bitmap (all variants have one).
    pub fn members(&self) -> &RoaringBitmap {
        match self {
            Self::Simple(b) | Self::Ordered { members: b, .. } | Self::Ranked { members: b, .. } => {
                b
            }
        }
    }

    /// Mutable borrow of the membership bitmap.
    pub fn members_mut(&mut self) -> &mut RoaringBitmap {
        match self {
            Self::Simple(b) | Self::Ordered { members: b, .. } | Self::Ranked { members: b, .. } => {
                b
            }
        }
    }

    /// Sequence (only for `Ordered`).
    pub fn sequence(&self) -> Option<&[ObjectId]> {
        match self {
            Self::Ordered { sequence, .. } => Some(sequence),
            _ => None,
        }
    }

    /// Ranked entries (only for `Ranked`).
    pub fn ranked(&self) -> Option<&[(ObjectId, i64)]> {
        match self {
            Self::Ranked { ranked, .. } => Some(ranked),
            _ => None,
        }
    }

    /// Insert `oid` into the membership bitmap. For `Ordered`, the sequence
    /// is **not** mutated — use [`TagStore::push_ordered`] for that. For
    /// `Ranked`, the score vector is **not** touched — use
    /// [`TagStore::insert_ranked`].
    pub fn add_member(&mut self, oid: ObjectId) {
        // Use bottom 32 bits as the roaring entry — see roaring's u32 limit;
        // this matches the "object local" interpretation used by the engine.
        let entry = (oid.to_u64() & 0xffff_ffff) as u32;
        self.members_mut().insert(entry);
    }

    /// Remove `oid` from the membership bitmap (and from any sequence /
    /// ranked vector). Returns `true` if it was present.
    pub fn remove_member(&mut self, oid: ObjectId) -> bool {
        let entry = (oid.to_u64() & 0xffff_ffff) as u32;
        let present = self.members_mut().remove(entry);
        match self {
            Self::Ordered { sequence, .. } => sequence.retain(|o| *o != oid),
            Self::Ranked { ranked, .. } => ranked.retain(|(o, _)| *o != oid),
            Self::Simple(_) => {}
        }
        present
    }

    /// Append `oid` to the ordered sequence (and to membership). No-op on
    /// non-`Ordered` variants.
    pub fn push_ordered(&mut self, oid: ObjectId) {
        if let Self::Ordered { members, sequence } = self {
            members.insert((oid.to_u64() & 0xffff_ffff) as u32);
            sequence.push(oid);
        }
    }

    /// Insert `(oid, score)` into the ranked store (and into membership).
    /// Vector is kept sorted by descending score. No-op on non-`Ranked`
    /// variants.
    pub fn insert_ranked(&mut self, oid: ObjectId, score: i64) {
        if let Self::Ranked { members, ranked } = self {
            members.insert((oid.to_u64() & 0xffff_ffff) as u32);
            ranked.push((oid, score));
            // Largest score first.
            ranked.sort_by(|a, b| b.1.cmp(&a.1));
        }
    }

    /// Total membership size.
    pub fn cardinality(&self) -> u64 {
        self.members().len()
    }

    /// `true` if no members.
    pub fn is_empty(&self) -> bool {
        self.members().is_empty()
    }

    /// Membership test.
    pub fn contains(&self, oid: ObjectId) -> bool {
        self.members().contains((oid.to_u64() & 0xffff_ffff) as u32)
    }

    /// Discriminant-style accessor for the on-disk `TagIndexLeafEntry::store_kind`
    /// byte.
    pub fn kind(&self) -> super::tag_index::TagStoreKind {
        match self {
            Self::Simple(_) => super::tag_index::TagStoreKind::Simple,
            Self::Ordered { .. } => super::tag_index::TagStoreKind::Ordered,
            Self::Ranked { .. } => super::tag_index::TagStoreKind::Ranked,
        }
    }

    /// Promote a `Simple` store into an `Ordered` one, preserving membership
    /// and using the bitmap's sorted order as the initial sequence.
    pub fn upgrade_to_ordered(&mut self) {
        if let Self::Simple(bm) = self {
            let mut members = RoaringBitmap::new();
            std::mem::swap(&mut members, bm);
            // Best-effort: synthesise ObjectIds with node_id = 0.
            let sequence: Vec<ObjectId> =
                members.iter().map(|x| ObjectId::from_u64(x as u64)).collect();
            *self = Self::Ordered { members, sequence };
        }
    }

    /// Promote a `Simple` (or `Ordered`) store into a `Ranked` one, with all
    /// scores defaulting to `0`. Membership is preserved.
    pub fn upgrade_to_ranked(&mut self) {
        let members = std::mem::take(self.members_mut());
        let ranked: Vec<(ObjectId, i64)> =
            members.iter().map(|x| (ObjectId::from_u64(x as u64), 0)).collect();
        *self = Self::Ranked { members, ranked };
    }
}

// ---------- Serde via the in-memory roaring bitmap CBOR fallback ----------

/// Serde proxy: roaring bitmaps don't derive `Serialize`/`Deserialize`, so
/// we go through the portable byte format (IMPL §13.1).
#[derive(Serialize, Deserialize)]
struct TagStoreSerde {
    kind: u8,
    bitmap_bytes: Vec<u8>,
    sequence: Vec<u64>,
    ranked: Vec<(u64, i64)>,
}

impl Serialize for TagStore {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let (kind, bm, seq, ranked) = match self {
            Self::Simple(bm) => (0u8, bm, vec![], vec![]),
            Self::Ordered { members, sequence } => (
                1u8,
                members,
                sequence.iter().map(|o| o.to_u64()).collect(),
                vec![],
            ),
            Self::Ranked { members, ranked } => (
                2u8,
                members,
                vec![],
                ranked.iter().map(|(o, s)| (o.to_u64(), *s)).collect(),
            ),
        };
        let mut bitmap_bytes = Vec::with_capacity(bm.serialized_size());
        bm.serialize_into(&mut bitmap_bytes)
            .map_err(serde::ser::Error::custom)?;
        TagStoreSerde {
            kind,
            bitmap_bytes,
            sequence: seq,
            ranked,
        }
        .serialize(ser)
    }
}

impl<'de> Deserialize<'de> for TagStore {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let p = TagStoreSerde::deserialize(de)?;
        let bm = RoaringBitmap::deserialize_from(p.bitmap_bytes.as_slice())
            .map_err(serde::de::Error::custom)?;
        match p.kind {
            0 => Ok(TagStore::Simple(bm)),
            1 => Ok(TagStore::Ordered {
                members: bm,
                sequence: p.sequence.into_iter().map(ObjectId::from_u64).collect(),
            }),
            2 => Ok(TagStore::Ranked {
                members: bm,
                ranked: p
                    .ranked
                    .into_iter()
                    .map(|(o, s)| (ObjectId::from_u64(o), s))
                    .collect(),
            }),
            other => Err(serde::de::Error::custom(format!(
                "unknown TagStore kind: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    #[test]
    fn simple_store_membership() {
        let mut s = TagStore::new_simple();
        s.add_member(oid(1));
        s.add_member(oid(7));
        assert!(s.contains(oid(1)));
        assert!(!s.contains(oid(2)));
        assert_eq!(s.cardinality(), 2);
        assert!(s.remove_member(oid(1)));
        assert!(!s.contains(oid(1)));
    }

    #[test]
    fn ordered_store_sequence() {
        let mut s = TagStore::new_ordered();
        s.push_ordered(oid(3));
        s.push_ordered(oid(1));
        s.push_ordered(oid(2));
        assert_eq!(
            s.sequence().unwrap(),
            &[oid(3), oid(1), oid(2)]
        );
        assert_eq!(s.cardinality(), 3);
    }

    #[test]
    fn ranked_store_sorted_descending() {
        let mut s = TagStore::new_ranked();
        s.insert_ranked(oid(1), 5);
        s.insert_ranked(oid(2), 9);
        s.insert_ranked(oid(3), 1);
        let r = s.ranked().unwrap();
        assert_eq!(r[0].0, oid(2));
        assert_eq!(r[1].0, oid(1));
        assert_eq!(r[2].0, oid(3));
    }

    #[test]
    fn upgrade_simple_to_ordered_preserves_members() {
        let mut s = TagStore::new_simple();
        s.add_member(oid(1));
        s.add_member(oid(2));
        s.upgrade_to_ordered();
        assert!(matches!(s, TagStore::Ordered { .. }));
        assert!(s.contains(oid(1)));
        assert!(s.contains(oid(2)));
    }

    #[test]
    fn upgrade_to_ranked_preserves_members() {
        let mut s = TagStore::new_simple();
        s.add_member(oid(1));
        s.upgrade_to_ranked();
        assert!(matches!(s, TagStore::Ranked { .. }));
        assert!(s.contains(oid(1)));
    }
}
