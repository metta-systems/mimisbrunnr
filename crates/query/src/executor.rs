//! Bitmap-algebra evaluator for [`mimisbrunnr_types::Query`].
//!
//! ## Bitmap encoding
//!
//! The result set type is `roaring::RoaringBitmap`, which is 32-bit. We pack
//! the *low 32 bits* of an [`ObjectId`]'s 48-bit local sequence into the
//! bitmap. For workloads with up to ~33 M objects on a single node this is
//! unambiguous; cluster mode (where `ObjectId.node` is non-zero) and
//! >4 G-objects pools both require a wider encoding.
//!
//! TODO(rewrite-phase-N): replace the 32-bit bitmap with a sharded encoding
//! that covers the full 48-bit local-id space and the 16-bit node prefix.
//!
//! ## Universe
//!
//! Several operators ([`Query::Not`], `CmpOp::Ne`) need a complement against
//! a "universe of all objects". We compute the universe lazily as the union
//! of every bitmap the [`TagIndex`] knows about. The forward index is *not*
//! consulted directly because it has the same membership view; the choice
//! is documented next to [`QueryExecutor::universe`].
//!
//! [`ObjectId`]: mimisbrunnr_types::ObjectId

use {
    log::trace,
    mimisbrunnr_index::{ForwardIndex, KvIndex, RangeIndex, TagIndex},
    mimisbrunnr_ontology::OntologyState,
    mimisbrunnr_types::{Assertion, CmpOp, ObjectId, Query, TagId, Value},
    roaring::RoaringBitmap,
};

use crate::error::QueryError;

/// The bitmap-algebra evaluator. Borrows the index mirrors and the ontology
/// state for `IsA` resolution.
pub struct QueryExecutor<'a> {
    /// Tag inverted index — primary source for `HasTag`, denominator for
    /// `Not` / `Ne`.
    pub tag_index: &'a TagIndex,
    /// KV equality index — services `HasAttr { op: Eq | Ne, .. }`.
    pub kv_index: &'a KvIndex,
    /// Range index — services `HasAttr { op: Lt | Le | Gt | Ge | Prefix, .. }`.
    pub range_index: &'a RangeIndex,
    /// Forward index — used by `Related` (linear scan) and to enumerate the
    /// distinct values an attribute key takes (`Ne`).
    pub forward_index: &'a ForwardIndex,
    /// Ontology state — used by `IsA` to expand into the closure of
    /// implied tags.
    pub ontology: &'a OntologyState,
}

impl<'a> QueryExecutor<'a> {
    /// Convenience constructor.
    pub fn new(
        tag_index: &'a TagIndex,
        kv_index: &'a KvIndex,
        range_index: &'a RangeIndex,
        forward_index: &'a ForwardIndex,
        ontology: &'a OntologyState,
    ) -> Self {
        Self {
            tag_index,
            kv_index,
            range_index,
            forward_index,
            ontology,
        }
    }

    /// Walk the [`Query`] tree and return the matching object set as a
    /// `RoaringBitmap` keyed by the low 32 bits of `ObjectId.local`.
    pub fn evaluate(&self, query: &Query) -> Result<RoaringBitmap, QueryError> {
        trace!("query::evaluate {query:?}");
        match query {
            Query::HasTag(tag) => Ok(self.eval_has_tag(*tag)),
            Query::HasAttr { key, op, value } => self.eval_has_attr(*key, *op, value),
            Query::Related { predicate, target } => Ok(self.eval_related(*predicate, *target)),
            Query::And(qs) => self.eval_and(qs),
            Query::Or(qs) => self.eval_or(qs),
            Query::Not(q) => self.eval_not(q),
            Query::IsA(tag) => Ok(self.eval_isa(*tag)),
        }
    }

    /// Evaluate and reconstruct full [`ObjectId`]s.
    ///
    /// Bitmap entries are the low 32 bits of the local sequence; this method
    /// rebuilds [`ObjectId`]s by zero-extending them to 48 bits and treating
    /// the node id as 0.
    ///
    /// TODO(rewrite-phase-N): once the bitmap representation grows beyond 32
    /// bits, plumb the node id through (likely via a side `objid_table` the
    /// engine maintains).
    pub fn evaluate_full(&self, query: &Query) -> Result<Vec<ObjectId>, QueryError> {
        let bm = self.evaluate(query)?;
        let mut out = Vec::with_capacity(bm.len() as usize);
        for entry in bm.iter() {
            // Single-node assumption: node = 0, local = entry as u64.
            out.push(ObjectId::from_parts(0, entry as u64));
        }
        Ok(out)
    }

    // ---------- Leaf operators ----------

    fn eval_has_tag(&self, tag: TagId) -> RoaringBitmap {
        self.tag_index
            .get(tag)
            .map(|store| store.members().clone())
            .unwrap_or_default()
    }

    fn eval_has_attr(
        &self,
        key: TagId,
        op: CmpOp,
        value: &Value,
    ) -> Result<RoaringBitmap, QueryError> {
        match op {
            CmpOp::Eq => Ok(self.kv_index.lookup(key, value)),
            CmpOp::Ne => {
                // Universe of objects with *any* assertion `Attr { key, .. }`,
                // minus the Eq set. We walk the forward index to build the
                // "any assertion for this key" bitmap. Linear in the number of
                // (object, assertion) pairs; replaced by a per-key reverse
                // index in a later phase.
                //
                // TODO(rewrite-phase-N): replace the forward-index scan with a
                // dedicated `tag_id → bitmap-of-objects-with-this-attr-key`
                // index.
                let any_with_key = self.objects_with_attr_key(key);
                let eq = self.kv_index.lookup(key, value);
                Ok(any_with_key - eq)
            }
            CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
                Ok(self.eval_range(key, op, value))
            }
            CmpOp::Prefix => Ok(self.eval_prefix(key, value)),
            CmpOp::Contains => {
                // Substring search needs a substring index we don't have yet.
                // The DESIGN/IMPL spec's fallback is a forward-index scan;
                // we deliberately surface this as an error rather than do a
                // silent O(N) linear scan that would mask the missing
                // capability.
                //
                // TODO(rewrite-phase-N): replace with a substring index or a
                // documented opt-in linear scan over `forward_index`.
                Err(QueryError::UnsupportedCmpOp { op: "Contains" })
            }
        }
    }

    /// Range-index scan for `Lt | Le | Gt | Ge`. Built on top of
    /// [`RangeIndex::range_scan`] (which is half-open `low ≤ k < high`).
    ///
    /// We use type-specific min/max sentinels for the unbounded side and
    /// an extra [`RangeIndex::lookup`] to land on inclusive boundaries.
    /// To handle the boundary case where `value` itself equals the type's
    /// max sentinel (e.g. `i64::MAX`), the sentinel is one step past the
    /// observable max; see [`type_max`] for the choice per `Value`
    /// variant.
    fn eval_range(&self, key: TagId, op: CmpOp, value: &Value) -> RoaringBitmap {
        let min = type_min(value);
        let max = type_max(value);
        let eq = self.range_index.lookup(key, value);
        match op {
            // [min, value) — strict `<`
            CmpOp::Lt => self.range_index.range_scan(key, &min, value),
            // [min, value) ∪ {value} — inclusive `<=`
            CmpOp::Le => {
                let mut acc = self.range_index.range_scan(key, &min, value);
                acc |= &eq;
                acc
            }
            // [value, max) ∖ {value} — strict `>`
            CmpOp::Gt => {
                let scan = self.range_index.range_scan(key, value, &max);
                scan - &eq
            }
            // [value, max) ∪ {value} — inclusive `>=`. Including `eq`
            // explicitly so an exact-`MAX` value still matches.
            CmpOp::Ge => {
                let mut acc = self.range_index.range_scan(key, value, &max);
                acc |= &eq;
                acc
            }
            _ => RoaringBitmap::new(),
        }
    }

    /// Prefix-match for `Value::Text`. Implemented as a forward-index linear
    /// scan: cheap for small pools, replaced by a dedicated index later.
    ///
    /// TODO(rewrite-phase-N): replace with a prefix-aware sub-index.
    fn eval_prefix(&self, key: TagId, value: &Value) -> RoaringBitmap {
        let needle = match value {
            Value::Text(s) => s.as_str(),
            _ => return RoaringBitmap::new(),
        };
        let mut acc = RoaringBitmap::new();
        for (oid, assertions) in self.forward_index.iter() {
            for (assertion, _origin) in assertions {
                if let Assertion::Attr {
                    key: k,
                    value: Value::Text(haystack),
                } = &assertion
                    && *k == key
                    && haystack.starts_with(needle)
                    && let Some(local32) = oid_low32(oid.to_u64())
                {
                    acc.insert(local32);
                }
            }
        }
        acc
    }

    /// Linear forward-index scan for `Related { predicate, target }`. Cost
    /// is O(forward index size); replaced by a relation-targeted reverse
    /// index in a later phase.
    ///
    /// TODO(rewrite-phase-N): replace with a `(predicate, target) → bitmap`
    /// reverse relation index.
    fn eval_related(&self, predicate: TagId, target: ObjectId) -> RoaringBitmap {
        let mut acc = RoaringBitmap::new();
        for (oid, assertions) in self.forward_index.iter() {
            for (assertion, _origin) in assertions {
                if let Assertion::Relation {
                    predicate: p,
                    target: t,
                } = &assertion
                    && *p == predicate
                    && *t == target
                    && let Some(local32) = oid_low32(oid.to_u64())
                {
                    acc.insert(local32);
                }
            }
        }
        acc
    }

    /// `IsA(tag)` per DESIGN §3.3 / §2.3.
    ///
    /// Although materialised tags are inserted at write time (so
    /// `HasTag(vehicle)` already covers cars), `IsA` is the explicit form:
    /// we expand `tag` via `OntologyState::materialise(&[tag])` (which
    /// returns the closure of implied tags) and union the membership
    /// bitmaps. The closure includes `tag` itself, so any object directly
    /// tagged with `tag` is included too.
    fn eval_isa(&self, tag: TagId) -> RoaringBitmap {
        let mut acc = RoaringBitmap::new();
        // `IsA(tag)` matches any object directly tagged with `tag` *or*
        // any of its descendants — i.e. tags whose closure includes
        // `tag`. We enumerate every tag the index has membership for and
        // consult `OntologyState::materialise` on it; if its closure
        // contains `tag`, its membership bitmap contributes to the
        // result. Per DESIGN §3.3, materialisation already happens at
        // write time, so `HasTag(vehicle)` would normally cover cars
        // already; `IsA(vehicle)` is the explicit form for queries
        // against pools whose ontology was installed *after* writes.
        for store_tag in self.tag_index.all_tags() {
            let closure = self.ontology.materialise(&[store_tag]);
            if closure.contains(&tag)
                && let Some(store) = self.tag_index.get(store_tag)
            {
                acc |= store.members();
            }
        }
        acc
    }

    // ---------- Combinators ----------

    fn eval_and(&self, qs: &[Query]) -> Result<RoaringBitmap, QueryError> {
        if qs.is_empty() {
            return Ok(RoaringBitmap::new());
        }
        // Smallest-first heuristic to keep the running intersection small.
        let mut bitmaps: Vec<RoaringBitmap> = qs
            .iter()
            .map(|q| self.evaluate(q))
            .collect::<Result<Vec<_>, _>>()?;
        bitmaps.sort_by_key(|b| b.len());
        let mut acc = bitmaps.swap_remove(0);
        for other in bitmaps {
            acc &= &other;
            if acc.is_empty() {
                break;
            }
        }
        Ok(acc)
    }

    fn eval_or(&self, qs: &[Query]) -> Result<RoaringBitmap, QueryError> {
        let mut acc = RoaringBitmap::new();
        for q in qs {
            acc |= self.evaluate(q)?;
        }
        Ok(acc)
    }

    fn eval_not(&self, q: &Query) -> Result<RoaringBitmap, QueryError> {
        let universe = self.universe();
        let inner = self.evaluate(q)?;
        Ok(universe - inner)
    }

    // ---------- Universe ----------

    /// "Universe of all objects" — the union of every membership bitmap the
    /// tag index knows about.
    ///
    /// Choice: we use the tag index rather than the forward index. They
    /// have the same conceptual coverage (every live object is tagged with
    /// at least one core tag like `file`), but the tag index is materialised
    /// as bitmaps already, so the union is roaring-fast (microseconds for
    /// thousands of tags). The forward index, in contrast, is keyed by
    /// `ObjectId` and would need a per-object iteration.
    fn universe(&self) -> RoaringBitmap {
        let mut acc = RoaringBitmap::new();
        for (_, store) in self.tag_index.iter() {
            acc |= store.members();
        }
        acc
    }

    /// Build the bitmap of all objects that carry an `Attr` assertion with
    /// `key = key`. Walks the forward index; cost is O(forward index size).
    fn objects_with_attr_key(&self, key: TagId) -> RoaringBitmap {
        let mut acc = RoaringBitmap::new();
        for (oid, assertions) in self.forward_index.iter() {
            for (assertion, _origin) in assertions {
                if let Assertion::Attr { key: k, .. } = &assertion
                    && *k == key
                    && let Some(local32) = oid_low32(oid.to_u64())
                {
                    acc.insert(local32);
                }
            }
        }
        acc
    }
}

/// Pack the low 32 bits of an `ObjectId.to_u64()` (which masks node into bits
/// 48..64) into a roaring entry. Returns `None` if the ID's low 32 bits would
/// alias the high 16 bits of the local sequence, signalling truncation.
fn oid_low32(oid_raw: u64) -> Option<u32> {
    // Local sequence occupies bits 0..48; the bitmap entry is bits 0..32.
    // Single-node clusters never set bits 32..48, so the cast is faithful.
    let local48 = oid_raw & 0x0000_ffff_ffff_ffff;
    if local48 > u32::MAX as u64 {
        // Caller-side TODO(rewrite-phase-N): widen the bitmap.
        None
    } else {
        Some(local48 as u32)
    }
}

// ---------- Type-extreme sentinels for range scans ----------
//
// The half-open `RangeIndex::range_scan(low, high)` covers `low ≤ k < high`.
// We need a bound that's strictly greater than any *observable* value of the
// requested type so that `>= value` and `> value` cover the entire upper
// tail. The encoding via `NormalisedKey` is what matters — numeric types
// embed an unsigned big-endian byte image, so picking `MAX` of the rust
// type gives a normalised key that's strictly greater than any observable
// value's NormalisedKey.
//
// For `Text` / `Blob` we pick a 13-byte string of `0xFF` and rely on the
// continuation flag in `NormalisedKey` (byte 14 = 1) to land above any
// real-world finite-length string.
//
// For `Scoped`, we recurse and reuse the inner type's sentinels under the
// same context tag.

fn type_min(value: &Value) -> Value {
    match value {
        Value::Int(_) => Value::Int(i64::MIN),
        Value::Float(_) => Value::Float(f64::NEG_INFINITY),
        Value::Timestamp(_) => Value::Timestamp(i64::MIN),
        Value::Text(_) => Value::Text(String::new()),
        Value::Blob(_) => Value::Blob(Vec::new()),
        Value::Scoped { context, inner } => Value::Scoped {
            context: *context,
            inner: Box::new(type_min(inner)),
        },
    }
}

fn type_max(value: &Value) -> Value {
    // For numeric types we use the rust max. The NormalisedKey encoding of
    // these maxes is the all-ones key, which compares strictly greater than
    // any observable value's key.
    match value {
        Value::Int(_) => Value::Int(i64::MAX),
        Value::Float(_) => Value::Float(f64::INFINITY),
        Value::Timestamp(_) => Value::Timestamp(i64::MAX),
        // For variable-length types pick a 0xFF-padded 13-byte payload;
        // continuation flag (byte 14 of NormalisedKey) is 1, which sorts
        // above any short / unflagged string.
        Value::Text(_) => Value::Text(String::from_utf8_lossy(&[0xFF; 32]).into_owned()),
        Value::Blob(_) => Value::Blob(vec![0xFF; 32]),
        Value::Scoped { context, inner } => Value::Scoped {
            context: *context,
            inner: Box::new(type_max(inner)),
        },
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        mimisbrunnr_index::{ForwardIndex, KvIndex, RangeIndex, TagIndex},
        mimisbrunnr_ontology::{IdAllocator, OntologyModule, OntologyState},
        mimisbrunnr_types::{ObjectId, TagId, TagOrigin, Value},
    };

    fn t(id: u32) -> TagId {
        TagId::new(id)
    }
    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    /// A bag of mock indices + ontology backing the executor.
    struct Fixture {
        tag: TagIndex,
        kv: KvIndex,
        range: RangeIndex,
        fwd: ForwardIndex,
        ont: OntologyState,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                tag: TagIndex::new(),
                kv: KvIndex::new(),
                range: RangeIndex::new(),
                fwd: ForwardIndex::new(),
                ont: OntologyState::new(),
            }
        }
        fn exec(&self) -> QueryExecutor<'_> {
            QueryExecutor::new(&self.tag, &self.kv, &self.range, &self.fwd, &self.ont)
        }
    }

    fn label(name: &str) -> mimisbrunnr_types::TagDefinition {
        mimisbrunnr_types::TagDefinition {
            id: TagId::new(0),
            name: name.into(),
            semantics: mimisbrunnr_types::TagSemantics::Label,
            implies: vec![],
            storage: None,
        }
    }

    fn install_chain(state: &mut OntologyState, names: &[&str], implications: &[(&str, &str)]) {
        let mut alloc = IdAllocator::new();
        let module = OntologyModule {
            id: "test".into(),
            version: "0.1.0".into(),
            name: "test".into(),
            tags: names.iter().map(|n| label(n)).collect(),
            implications: implications
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            relations: vec![],
        };
        state.install(module, &mut alloc).unwrap();
    }

    fn bm_to_vec(bm: &RoaringBitmap) -> Vec<u32> {
        bm.iter().collect()
    }

    #[test]
    fn has_tag_round_trip() {
        let mut f = Fixture::new();
        f.tag.add_member(t(1), oid(1));
        f.tag.add_member(t(1), oid(2));
        f.tag.add_member(t(1), oid(3));
        f.tag.add_member(t(2), oid(2));
        f.tag.add_member(t(2), oid(3));
        f.tag.add_member(t(2), oid(4));

        let r = f.exec().evaluate(&Query::HasTag(t(1))).unwrap();
        assert_eq!(bm_to_vec(&r), vec![1, 2, 3]);
    }

    #[test]
    fn and_intersection() {
        let mut f = Fixture::new();
        f.tag.add_member(t(1), oid(1));
        f.tag.add_member(t(1), oid(2));
        f.tag.add_member(t(1), oid(3));
        f.tag.add_member(t(2), oid(2));
        f.tag.add_member(t(2), oid(3));
        f.tag.add_member(t(2), oid(4));

        let q = Query::And(vec![Query::HasTag(t(1)), Query::HasTag(t(2))]);
        let r = f.exec().evaluate(&q).unwrap();
        assert_eq!(bm_to_vec(&r), vec![2, 3]);
    }

    #[test]
    fn or_union() {
        let mut f = Fixture::new();
        f.tag.add_member(t(1), oid(1));
        f.tag.add_member(t(1), oid(2));
        f.tag.add_member(t(1), oid(3));
        f.tag.add_member(t(2), oid(2));
        f.tag.add_member(t(2), oid(3));
        f.tag.add_member(t(2), oid(4));

        let q = Query::Or(vec![Query::HasTag(t(1)), Query::HasTag(t(2))]);
        let r = f.exec().evaluate(&q).unwrap();
        assert_eq!(bm_to_vec(&r), vec![1, 2, 3, 4]);
    }

    #[test]
    fn not_against_universe() {
        let mut f = Fixture::new();
        for i in 1..=4 {
            f.tag.add_member(t(0), oid(i));
        }
        f.tag.add_member(t(1), oid(1));
        f.tag.add_member(t(1), oid(2));
        f.tag.add_member(t(1), oid(3));

        // Universe = {1,2,3,4} ; Not(HasTag(1)) = {4}
        let q = Query::Not(Box::new(Query::HasTag(t(1))));
        let r = f.exec().evaluate(&q).unwrap();
        assert_eq!(bm_to_vec(&r), vec![4]);
    }

    #[test]
    fn not_on_empty_universe_yields_empty() {
        let f = Fixture::new();
        let q = Query::Not(Box::new(Query::HasTag(t(99))));
        let r = f.exec().evaluate(&q).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn has_attr_eq_round_trip() {
        let mut f = Fixture::new();
        let year = t(10);
        f.kv.insert(year, &Value::Int(2024), 1);
        f.kv.insert(year, &Value::Int(2024), 2);
        f.kv.insert(year, &Value::Int(2023), 3);

        let q = Query::HasAttr {
            key: year,
            op: CmpOp::Eq,
            value: Value::Int(2024),
        };
        let r = f.exec().evaluate(&q).unwrap();
        assert_eq!(bm_to_vec(&r), vec![1, 2]);
    }

    #[test]
    fn has_attr_lt_via_range_index() {
        let mut f = Fixture::new();
        let year = t(10);
        for y in 2020..2025i64 {
            f.range.insert(year, &Value::Int(y), y as u32);
        }
        let q = Query::HasAttr {
            key: year,
            op: CmpOp::Lt,
            value: Value::Int(2024),
        };
        let r = f.exec().evaluate(&q).unwrap();
        // 2020,2021,2022,2023 — bitmap entries equal those years.
        let mut got = bm_to_vec(&r);
        got.sort();
        assert_eq!(got, vec![2020, 2021, 2022, 2023]);
    }

    #[test]
    fn has_attr_gt_via_range_index() {
        let mut f = Fixture::new();
        let year = t(10);
        for y in 2020..2025i64 {
            f.range.insert(year, &Value::Int(y), y as u32);
        }
        let q = Query::HasAttr {
            key: year,
            op: CmpOp::Gt,
            value: Value::Int(2022),
        };
        let r = f.exec().evaluate(&q).unwrap();
        let mut got = bm_to_vec(&r);
        got.sort();
        assert_eq!(got, vec![2023, 2024]);
    }

    #[test]
    fn has_attr_ne() {
        let mut f = Fixture::new();
        let key = t(10);
        // forward index records `Attr { key, value }` for three objects:
        // obj1=2024, obj2=2024, obj3=2023
        f.fwd.add_assertion(
            oid(1),
            Assertion::Attr {
                key,
                value: Value::Int(2024),
            },
            TagOrigin::Direct,
        );
        f.fwd.add_assertion(
            oid(2),
            Assertion::Attr {
                key,
                value: Value::Int(2024),
            },
            TagOrigin::Direct,
        );
        f.fwd.add_assertion(
            oid(3),
            Assertion::Attr {
                key,
                value: Value::Int(2023),
            },
            TagOrigin::Direct,
        );
        // KV index has matching state for the Eq part:
        f.kv.insert(key, &Value::Int(2024), 1);
        f.kv.insert(key, &Value::Int(2024), 2);
        f.kv.insert(key, &Value::Int(2023), 3);

        let q = Query::HasAttr {
            key,
            op: CmpOp::Ne,
            value: Value::Int(2024),
        };
        let r = f.exec().evaluate(&q).unwrap();
        assert_eq!(bm_to_vec(&r), vec![3]);
    }

    #[test]
    fn contains_returns_unsupported() {
        let f = Fixture::new();
        let q = Query::HasAttr {
            key: t(1),
            op: CmpOp::Contains,
            value: Value::Text("foo".into()),
        };
        let err = f.exec().evaluate(&q).unwrap_err();
        assert!(matches!(err, QueryError::UnsupportedCmpOp { op: "Contains" }));
    }

    #[test]
    fn isa_resolves_descendants() {
        let mut f = Fixture::new();
        // ontology: car implies vehicle.
        install_chain(&mut f.ont, &["car", "vehicle"], &[("car", "vehicle")]);
        let car = f.ont.names["car"];
        let vehicle = f.ont.names["vehicle"];

        // obj1 directly tagged car; obj2 directly tagged vehicle.
        f.tag.add_member(car, oid(1));
        f.tag.add_member(vehicle, oid(2));

        // IsA(vehicle) => {1, 2} — obj1 is_a vehicle via car, obj2 directly.
        let r = f.exec().evaluate(&Query::IsA(vehicle)).unwrap();
        let mut got = bm_to_vec(&r);
        got.sort();
        assert_eq!(got, vec![1, 2]);
    }

    #[test]
    fn related_linear_scan() {
        let mut f = Fixture::new();
        let pred = t(7);
        let target = oid(900);
        f.fwd.add_assertion(
            oid(1),
            Assertion::Relation {
                predicate: pred,
                target,
            },
            TagOrigin::Direct,
        );
        f.fwd.add_assertion(
            oid(2),
            Assertion::Relation {
                predicate: pred,
                target: oid(901), // different target
            },
            TagOrigin::Direct,
        );
        let q = Query::Related {
            predicate: pred,
            target,
        };
        let r = f.exec().evaluate(&q).unwrap();
        assert_eq!(bm_to_vec(&r), vec![1]);
    }

    #[test]
    fn evaluate_full_reconstructs_object_ids() {
        let mut f = Fixture::new();
        f.tag.add_member(t(1), oid(10));
        f.tag.add_member(t(1), oid(20));
        let ids = f.exec().evaluate_full(&Query::HasTag(t(1))).unwrap();
        let mut locals: Vec<u64> = ids.iter().map(|o| o.local_seq()).collect();
        locals.sort();
        assert_eq!(locals, vec![10, 20]);
    }

    #[test]
    fn complex_query_from_design_doc() {
        // "all portable electronics from 2024, not discontinued"
        let mut f = Fixture::new();
        let electronics = t(1);
        let portable = t(2);
        let discontinued = t(3);
        let year = t(10);

        for id in 1..=10u64 {
            f.tag.add_member(electronics, oid(id));
        }
        for id in [3u64, 5, 7, 8] {
            f.tag.add_member(portable, oid(id));
        }
        for id in [5u32, 7, 8, 9] {
            f.kv.insert(year, &Value::Int(2024), id);
        }
        f.tag.add_member(discontinued, oid(7));

        let q = Query::And(vec![
            Query::HasTag(electronics),
            Query::HasTag(portable),
            Query::HasAttr {
                key: year,
                op: CmpOp::Eq,
                value: Value::Int(2024),
            },
            Query::Not(Box::new(Query::HasTag(discontinued))),
        ]);
        let r = f.exec().evaluate(&q).unwrap();
        let mut got = bm_to_vec(&r);
        got.sort();
        assert_eq!(got, vec![5, 8]);
    }
}
