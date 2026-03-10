use {arbitrary_int::u48, bitbybit::bitfield};

/// Globally unique object identifier.
///
/// Top 16 bits encode the originating node ID, bottom 48 bits are
/// a monotonically increasing local sequence number. This allows
/// independent creation across cluster nodes with zero coordination.
#[bitfield(u64)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId {
    #[bits(0..=47, rw)]
    local: u48,
    #[bits(48..=63, rw)]
    node: u16,
}

impl ObjectId {
    pub fn new(node: u16, local_seq: u48) -> Self {
        Self::builder()
            .with_node(node)
            .with_local(local_seq)
            .build()
    }

    pub fn from_raw(raw: u64) -> Self {
        Self::new_with_raw_value(raw)
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
        let id = ObjectId::new(42, u48::from_u64(1_000_000));
        assert_eq!(id.node(), 42);
        assert_eq!(id.local(), u48::from_u64(1_000_000));
    }

    #[test]
    fn zero_node() {
        let id = ObjectId::new(0, u48::from_u64(123));
        assert_eq!(id.node(), 0);
        assert_eq!(id.local(), u48::from_u64(123));
    }

    #[test]
    fn max_node() {
        let id = ObjectId::new(u16::MAX, u48::from_u64(0));
        assert_eq!(id.node(), u16::MAX);
        assert_eq!(id.local(), u48::from_u64(0));
    }

    #[test]
    fn max_local() {
        let max_local = u48::from_u64(0xffff_ffff_ffff);
        let id = ObjectId::new(1, max_local);
        assert_eq!(id.node(), 1);
        assert_eq!(id.local(), max_local);
    }

    #[test]
    fn raw_round_trip() {
        let id = ObjectId::new(7, u48::from_u64(999));
        let raw = id.raw_value();
        assert_eq!(ObjectId::from_raw(raw), id);
    }

    #[test]
    fn display_format() {
        let id = ObjectId::new(1, u48::from_u64(42));
        assert_eq!(format!("{id}"), "obj:1:42");
    }

    #[test]
    fn ordering() {
        let a = ObjectId::new(1, u48::from_u64(1));
        let b = ObjectId::new(1, u48::from_u64(2));
        let c = ObjectId::new(2, u48::from_u64(1));
        assert!(a < b);
        assert!(b < c);
    }
}
