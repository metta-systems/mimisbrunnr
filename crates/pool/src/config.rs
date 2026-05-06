//! Per-pool TOML configuration (`pool.toml`).
//!
//! DESIGN §15 — links `brunnr` and `mimir` to the same pool by listing the
//! disks that compose it.
//!
//! Example:
//!
//! ```toml
//! node_id = 1
//!
//! [[disks]]
//! id = 1
//! path = "/var/mimir/disk0.img"
//! media_type = "Nvme"
//! tier = "Hot"
//! capacity_bytes = 17179869184
//!
//! [[disks]]
//! id = 2
//! path = "/var/mimir/disk1.img"
//! media_type = "Hdd"
//! tier = "Cold"
//! capacity_bytes = 549755813888
//! ```

use std::path::{Path, PathBuf};

use {
    log::trace,
    mimisbrunnr_types::{DiskId, MediaType, NodeId, StorageTier},
    serde::{Deserialize, Serialize},
};

use crate::error::PoolError;

/// One disk's row in `pool.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskConfigEntry {
    pub id: DiskId,
    pub path: PathBuf,
    pub media_type: MediaType,
    pub tier: StorageTier,
    pub capacity_bytes: u64,
}

/// On-disk pool configuration as serialised in `pool.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolConfig {
    pub node_id: NodeId,
    pub disks: Vec<DiskConfigEntry>,
}

impl PoolConfig {
    /// Construct a fresh `PoolConfig` for `node_id` with no disks yet.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            disks: Vec::new(),
        }
    }

    /// Read a `pool.toml` from disk.
    pub fn load_toml(path: &Path) -> Result<Self, PoolError> {
        trace!("PoolConfig::load_toml path={}", path.display());
        let body = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&body)?;
        Ok(cfg)
    }

    /// Write this config out as `pool.toml`. The file is fully replaced.
    pub fn save_toml(&self, path: &Path) -> Result<(), PoolError> {
        trace!("PoolConfig::save_toml path={}", path.display());
        let body = toml::to_string_pretty(self)?;
        std::fs::write(path, body)?;
        Ok(())
    }

    /// Return the entry that matches `id`, if any.
    pub fn find_disk(&self, id: DiskId) -> Option<&DiskConfigEntry> {
        self.disks.iter().find(|d| d.id == id)
    }

    /// The "primary" disk — the lowest disk id in the pool. The primary
    /// disk holds the `PoolStateRoot` block in its metadata zone.
    pub fn primary(&self) -> Option<&DiskConfigEntry> {
        self.disks.iter().min_by_key(|d| d.id)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, tempfile::TempDir};

    fn sample() -> PoolConfig {
        PoolConfig {
            node_id: 1,
            disks: vec![
                DiskConfigEntry {
                    id: 1,
                    path: PathBuf::from("/var/mimir/disk0.img"),
                    media_type: MediaType::NVMe,
                    tier: StorageTier::Hot,
                    capacity_bytes: 17_179_869_184,
                },
                DiskConfigEntry {
                    id: 2,
                    path: PathBuf::from("/var/mimir/disk1.img"),
                    media_type: MediaType::Hdd,
                    tier: StorageTier::Cold,
                    capacity_bytes: 549_755_813_888,
                },
            ],
        }
    }

    #[test]
    fn toml_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pool.toml");
        let cfg = sample();
        cfg.save_toml(&path).unwrap();
        let back = PoolConfig::load_toml(&path).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn primary_picks_lowest_id() {
        let cfg = sample();
        assert_eq!(cfg.primary().unwrap().id, 1);
    }

    #[test]
    fn find_disk_lookup() {
        let cfg = sample();
        assert!(cfg.find_disk(1).is_some());
        assert!(cfg.find_disk(99).is_none());
    }

    #[test]
    fn missing_file_errors() {
        let err = PoolConfig::load_toml(Path::new("/no/such/pool.toml")).unwrap_err();
        assert!(matches!(err, PoolError::Io(_)));
    }
}
