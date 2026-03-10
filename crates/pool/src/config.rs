use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PoolError;
use log::trace;

/// On-disk pool configuration, persisted as TOML alongside the pool.
///
/// Stored at `<pool_root>/pool.toml` and tells both brunnr and mimir
/// which disk files belong to the pool and their roles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    pub node_id: u16,
    pub disks: Vec<DiskEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskEntry {
    pub id: u16,
    pub path: String,
    pub tier: String,
    pub capacity_bytes: u64,
}

impl PoolConfig {
    pub fn new(node_id: u16) -> Self {
        Self {
            node_id,
            disks: Vec::new(),
        }
    }

    pub fn add_disk(&mut self, id: u16, path: impl Into<String>, tier: &str, capacity: u64) {
        self.disks.push(DiskEntry {
            id,
            path: path.into(),
            tier: tier.to_string(),
            capacity_bytes: capacity,
        });
    }

    /// Write config to a TOML file.
    pub fn save(&self, path: &Path) -> Result<(), PoolError> {
        trace!("PoolConfig::save path={}", path.display());
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| PoolError::ConfigError(e.to_string()))?;
        std::fs::write(path, toml_str).map_err(PoolError::Io)?;
        Ok(())
    }

    /// Load config from a TOML file.
    pub fn load(path: &Path) -> Result<Self, PoolError> {
        trace!("PoolConfig::load path={}", path.display());
        let contents = std::fs::read_to_string(path).map_err(PoolError::Io)?;
        let config: Self =
            toml::from_str(&contents).map_err(|e| PoolError::ConfigError(e.to_string()))?;
        Ok(config)
    }

    /// Find the pool.toml by searching upward from a given disk path,
    /// or by looking in the same directory.
    pub fn find_from_disk(disk_path: &Path) -> Result<(PathBuf, Self), PoolError> {
        trace!("PoolConfig::find_from_disk disk={}", disk_path.display());
        // Try same directory as the disk file
        if let Some(parent) = disk_path.parent() {
            let config_path = parent.join("pool.toml");
            if config_path.exists() {
                let config = Self::load(&config_path)?;
                return Ok((config_path, config));
            }
        }
        Err(PoolError::ConfigNotFound(
            disk_path.to_string_lossy().to_string(),
        ))
    }

    /// Get the directory containing the pool config.
    pub fn pool_dir(config_path: &Path) -> Option<&Path> {
        config_path.parent()
    }

    /// Primary disk (disk 0 — holds index + metadata zones).
    pub fn primary_disk(&self) -> Option<&DiskEntry> {
        self.disks.iter().find(|d| d.id == 0)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, tempfile::TempDir};

    #[test]
    fn round_trip_config() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("pool.toml");

        let mut config = PoolConfig::new(42);
        config.add_disk(0, "/tmp/disk0.mbrunnr", "hot", 128 * 1024 * 1024);
        config.add_disk(1, "/tmp/disk1.mbrunnr", "warm", 256 * 1024 * 1024);

        config.save(&config_path).unwrap();

        let loaded = PoolConfig::load(&config_path).unwrap();
        assert_eq!(loaded.node_id, 42);
        assert_eq!(loaded.disks.len(), 2);
        assert_eq!(loaded.disks[0].path, "/tmp/disk0.mbrunnr");
        assert_eq!(loaded.disks[0].tier, "hot");
        assert_eq!(loaded.disks[1].capacity_bytes, 256 * 1024 * 1024);
    }

    #[test]
    fn find_from_disk() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("pool.toml");
        let disk_path = tmp.path().join("disk0.mbrunnr");

        let mut config = PoolConfig::new(0);
        config.add_disk(0, disk_path.to_str().unwrap(), "warm", 1000);
        config.save(&config_path).unwrap();

        // Create the disk file so the path exists
        std::fs::write(&disk_path, b"").unwrap();

        let (found_path, found_config) = PoolConfig::find_from_disk(&disk_path).unwrap();
        assert_eq!(found_path, config_path);
        assert_eq!(found_config.node_id, 0);
    }

    #[test]
    fn primary_disk() {
        let mut config = PoolConfig::new(0);
        config.add_disk(1, "disk1", "warm", 1000);
        config.add_disk(0, "disk0", "hot", 2000);

        let primary = config.primary_disk().unwrap();
        assert_eq!(primary.id, 0);
        assert_eq!(primary.path, "disk0");
    }

    #[test]
    fn config_not_found() {
        let result = PoolConfig::find_from_disk(Path::new("/nonexistent/disk"));
        assert!(result.is_err());
    }
}
