/// Identifier for a tag definition in the ontology.
///
/// 32-bit, allowing up to ~4 billion unique tags across the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TagId(u32);

impl TagId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    pub fn raw(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for TagId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tag:{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let t = TagId::new(42);
        assert_eq!(t.raw(), 42);
    }

    #[test]
    fn ordering() {
        assert!(TagId::new(1) < TagId::new(2));
    }

    #[test]
    fn display() {
        assert_eq!(format!("{}", TagId::new(99)), "tag:99");
    }
}
