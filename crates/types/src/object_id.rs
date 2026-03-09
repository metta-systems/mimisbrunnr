/// Globally unique object identifier.
///
/// Top 16 bits encode the originating node ID, bottom 48 bits are
/// a monotonically increasing local sequence number. This allows
/// independent creation across cluster nodes with zero coordination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(u64);

const LOCAL_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

impl ObjectId {
    pub fn new(node: u16, local_seq: u64) -> Self {
        debug_assert!(
            local_seq <= LOCAL_MASK,
            "local_seq exceeds 48-bit range"
        );
        Self((node as u64) << 48 | (local_seq & LOCAL_MASK))
    }

    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn raw(self) -> u64 {
        self.0
    }

    pub fn node(self) -> u16 {
        (self.0 >> 48) as u16
    }

    pub fn local(self) -> u64 {
        self.0 & LOCAL_MASK
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "obj:{}:{}", self.node(), self.local())
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
        let id = ObjectId::new(u16::MAX, 0);
        assert_eq!(id.node(), u16::MAX);
        assert_eq!(id.local(), 0);
    }

    #[test]
    fn max_local() {
        let max_local = LOCAL_MASK;
        let id = ObjectId::new(1, max_local);
        assert_eq!(id.node(), 1);
        assert_eq!(id.local(), max_local);
    }

    #[test]
    fn raw_round_trip() {
        let id = ObjectId::new(7, 999);
        let raw = id.raw();
        assert_eq!(ObjectId::from_raw(raw), id);
    }

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
