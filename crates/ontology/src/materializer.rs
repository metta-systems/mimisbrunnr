//! Materializer (DESIGN §3.3).
//!
//! Thin wrapper around [`ImplicationDag`] exposing the public materialisation
//! API. Returns sets; does **not** touch any index. The engine wires the
//! materialiser's output into the index at write time.

use mimisbrunnr_types::TagId;

use crate::implication_dag::ImplicationDag;

/// Computes materialised tag sets and answers `is_a` queries against an
/// [`ImplicationDag`].
pub struct Materializer<'a> {
    dag: &'a ImplicationDag,
}

impl<'a> Materializer<'a> {
    pub fn new(dag: &'a ImplicationDag) -> Self {
        Self { dag }
    }

    /// Compute the materialised tag set for an object whose direct tag set is
    /// `direct`. The result includes the direct tags plus every tag reachable
    /// via implication edges, sorted by `TagId` for determinism.
    pub fn materialise(&self, direct: &[TagId]) -> Vec<TagId> {
        self.dag.closure(direct)
    }

    /// `true` iff `tag` directly or transitively implies `ancestor`. Backs
    /// `Query::IsA` evaluation.
    pub fn is_a(&self, tag: TagId, ancestor: TagId) -> bool {
        self.dag.is_a(tag, ancestor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u32) -> TagId {
        TagId::new(id)
    }

    fn vehicle_dag() -> ImplicationDag {
        let mut d = ImplicationDag::new();
        for id in 1..=4 {
            d.add_tag(t(id));
        }
        // 1=car, 2=truck, 3=vehicle, 4=physical_object
        d.add_implication(t(1), t(3)).unwrap();
        d.add_implication(t(2), t(3)).unwrap();
        d.add_implication(t(3), t(4)).unwrap();
        d
    }

    #[test]
    fn materialise_car_includes_ancestors() {
        let d = vehicle_dag();
        let m = Materializer::new(&d);
        assert_eq!(m.materialise(&[t(1)]), vec![t(1), t(3), t(4)]);
    }

    #[test]
    fn materialise_dedupes_overlap() {
        let d = vehicle_dag();
        let m = Materializer::new(&d);
        // car + truck both imply vehicle → physical_object
        assert_eq!(m.materialise(&[t(1), t(2)]), vec![t(1), t(2), t(3), t(4)]);
    }

    #[test]
    fn is_a_query() {
        let d = vehicle_dag();
        let m = Materializer::new(&d);
        assert!(m.is_a(t(1), t(3)));
        assert!(m.is_a(t(1), t(4)));
        assert!(!m.is_a(t(3), t(1)));
    }
}
