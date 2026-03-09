/// Storage tier classification based on performance characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum StorageTier {
    /// Fastest storage (NVMe). Active/scratch data.
    Hot = 0,
    /// Medium performance (SSD). Default placement.
    Warm = 1,
    /// Slow storage (HDD). Archives, backups.
    Cold = 2,
    /// Remote/offline storage (S3, SFTP). Deep archive.
    Glacier = 3,
}

impl StorageTier {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Hot),
            1 => Some(Self::Warm),
            2 => Some(Self::Cold),
            3 => Some(Self::Glacier),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Warm => "warm",
            Self::Cold => "cold",
            Self::Glacier => "glacier",
        }
    }
}

impl std::fmt::Display for StorageTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for v in 0..=3u8 {
            let tier = StorageTier::from_u8(v).unwrap();
            assert_eq!(tier as u8, v);
        }
        assert!(StorageTier::from_u8(4).is_none());
    }

    #[test]
    fn ordering() {
        assert!(StorageTier::Hot < StorageTier::Warm);
        assert!(StorageTier::Warm < StorageTier::Cold);
        assert!(StorageTier::Cold < StorageTier::Glacier);
    }

    #[test]
    fn display() {
        assert_eq!(format!("{}", StorageTier::Hot), "hot");
        assert_eq!(format!("{}", StorageTier::Glacier), "glacier");
    }
}
