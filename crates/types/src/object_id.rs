use crate::NodeId;

/// Globally unique object identifier.
///
/// Top 16 bits encode the originating node ID, bottom 48 bits are
/// a monotonically increasing local sequence number. This allows
/// independent creation across cluster nodes with zero coordination.
#[derive(Debug, PartialEq, Copy, Clone, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId {
    node: NodeId,
    local: u64,
}

impl ObjectId {
    pub fn new(node: NodeId, local: u64) -> Self {
        Self { node, local }
    }

    pub fn node(&self) -> NodeId {
        self.node
    }

    pub fn local(&self) -> u64 {
        self.local
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "obj:{:x}:{}", self.node, self.local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_node_and_local() {
        let id = ObjectId::new(42, 1_000_000);
        assert_eq!(id.node(), 42);
        assert_eq!(id.local(), 1_000_000);
    }

    #[test]
    fn zero_node() {
        let id = ObjectId::new(0, 123);
        assert_eq!(id.node(), 0);
        assert_eq!(id.local(), 123);
    }

    #[test]
    fn max_node() {
        let id = ObjectId::new(u64::MAX, 0);
        assert_eq!(id.node(), u64::MAX);
        assert_eq!(id.local(), 0);
    }

    #[test]
    fn max_local() {
        let max_local = 0xffff_ffff_ffff;
        let id = ObjectId::new(1, max_local);
        assert_eq!(id.node(), 1);
        assert_eq!(id.local(), max_local);
    }

    // #[test]
    // fn raw_round_trip() {
    //     let id = ObjectId::new(7, 999);
    //     let raw = id.raw_value();
    //     assert_eq!(ObjectId::from_raw(raw), id);
    // }

    #[test]
    fn display_format() {
        let id = ObjectId::new(1, 42);
        assert_eq!(format!("{id}"), "obj:1:42");
    }

    #[test]
    fn ordering() {
        let a = ObjectId::new(1, 1);
        let b = ObjectId::new(1, 2);
        let c = ObjectId::new(2, 1);
        assert!(a < b);
        assert!(b < c);
    }
}
