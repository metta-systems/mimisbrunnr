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
    /// Default compression for objects not matching any Compress rule.
    /// Format: "none", "zstd:LEVEL" (e.g. "zstd:3"), "lz4".
    #[serde(default = "default_compression")]
    pub default_compression: String,
    /// Placement rules (serialized as TOML-friendly records).
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
}

fn default_compression() -> String {
    "zstd:3".to_string()
}

/// A placement rule in TOML-serializable form.
///
/// Query strings are resolved against the ontology DAG at load time.
/// Example:
/// ```toml
/// [[rules]]
/// type = "compress"
/// query = "video OR image"
/// algo = "none"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleConfig {
    /// Rule type: "compress", "pin", "prefer", "replicate", "colocate", "auto-tier".
    #[serde(rename = "type")]
    pub rule_type: String,
    /// Query string to match objects (not used for auto-tier).
    #[serde(default)]
    pub query: Option<String>,
    /// Compression algorithm: "none", "zstd:LEVEL", "lz4" (for compress rules).
    #[serde(default)]
    pub algo: Option<String>,
    /// Storage tier: "hot", "warm", "cold", "glacier" (for pin/prefer rules).
    #[serde(default)]
    pub tier: Option<String>,
    /// Priority 0-255 (for prefer rules).
    #[serde(default)]
    pub priority: Option<u8>,
    /// Minimum replica count (for replicate rules).
    #[serde(default)]
    pub min_replicas: Option<u8>,
    /// Whether to replicate across different disks (for replicate rules).
    #[serde(default)]
    pub across_disks: Option<bool>,
    /// Hot threshold in days (for auto-tier rules).
    #[serde(default)]
    pub hot_threshold_days: Option<u32>,
    /// Warm threshold in days (for auto-tier rules).
    #[serde(default)]
    pub warm_threshold_days: Option<u32>,
    /// Cold-after threshold in days (for auto-tier rules).
    #[serde(default)]
    pub cold_after: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskEntry {
    pub id: u16,
    pub path: String,
    pub tier: String,
    pub capacity_bytes: u64,
}

/// Parse a compression algo string like "none", "zstd:3", "lz4".
pub fn parse_compression_algo(s: &str) -> Option<mimisbrunnr_transform::CompressionAlgo> {
    use mimisbrunnr_transform::CompressionAlgo;
    let s = s.trim().to_lowercase();
    if s == "none" || s == "skip" {
        Some(CompressionAlgo::None)
    } else if s == "lz4" {
        Some(CompressionAlgo::Lz4)
    } else if let Some(rest) = s.strip_prefix("zstd") {
        let level = if let Some(level_str) = rest.strip_prefix(':') {
            level_str.trim().parse::<i32>().ok()?
        } else {
            3 // default zstd level
        };
        Some(CompressionAlgo::Zstd(level))
    } else {
        None
    }
}

impl PoolConfig {
    pub fn new(node_id: u16) -> Self {
        Self {
            node_id,
            disks: Vec::new(),
            default_compression: default_compression(),
            rules: Vec::new(),
        }
    }

    /// Parse the default_compression field into a CompressionAlgo.
    pub fn default_compression_algo(&self) -> mimisbrunnr_transform::CompressionAlgo {
        parse_compression_algo(&self.default_compression)
            .unwrap_or(mimisbrunnr_transform::CompressionAlgo::Zstd(3))
    }

    pub fn add_disk(&mut self, id: u16, path: impl Into<String>, tier: &str, capacity: u64) {
        self.disks.push(DiskEntry {
            id,
            path: path.into(),
            tier: tier.to_string(),
            capacity_bytes: capacity,
        });
    }

    /// Add a compression rule in config format.
    pub fn add_compress_rule(&mut self, query: &str, algo: &str) {
        self.rules.push(RuleConfig {
            rule_type: "compress".to_string(),
            query: Some(query.to_string()),
            algo: Some(algo.to_string()),
            tier: None,
            priority: None,
            min_replicas: None,
            across_disks: None,
            hot_threshold_days: None,
            warm_threshold_days: None,
            cold_after: None,
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
