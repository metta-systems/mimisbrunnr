//! Content presence / hydration model (DESIGN §10.3).
//!
//! Captures whether an object's payload is locally resident, remote-only,
//! actively hydrating, cached on a local tier, or partially materialised
//! (a stub).

use serde::{Deserialize, Serialize};

use crate::ids::{DiskId, NodeId};

/// Where the payload bytes for an object currently live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentPresence {
    /// Fully resident on the named disk.
    Local {
        disk_id: DiskId,
        extent_offset: u64,
        extent_length: u64,
    },
    /// Resident only on remote node(s); not on any local disk.
    Remote {
        origin_node: NodeId,
        mirrors: Vec<NodeId>,
    },
    /// Currently being fetched.
    Hydrating {
        source: NodeId,
        progress_bytes: u64,
        total_bytes: u64,
    },
    /// Cached locally with a fetch-time stamp for eviction.
    Cached {
        disk_id: DiskId,
        extent_offset: u64,
        /// Fetch time as nanoseconds since the Unix epoch.
        fetched_at_ns: i64,
    },
    /// Stub: only `stub_length` bytes are present locally; the full payload
    /// is `full_length` bytes long and lives on `origin_node`.
    Partial {
        stub_length: u64,
        full_length: u64,
        origin_node: NodeId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbor_round_trip_all_variants() {
        let variants = [
            ContentPresence::Local {
                disk_id: 1,
                extent_offset: 4096,
                extent_length: 1 << 20,
            },
            ContentPresence::Remote {
                origin_node: 7,
                mirrors: vec![8, 9],
            },
            ContentPresence::Hydrating {
                source: 7,
                progress_bytes: 1024,
                total_bytes: 1 << 20,
            },
            ContentPresence::Cached {
                disk_id: 0,
                extent_offset: 0,
                fetched_at_ns: 1_700_000_000_000_000_000,
            },
            ContentPresence::Partial {
                stub_length: 4096,
                full_length: 1 << 30,
                origin_node: 7,
            },
        ];
        for v in variants {
            let mut buf = Vec::new();
            ciborium::ser::into_writer(&v, &mut buf).unwrap();
            let back: ContentPresence = ciborium::de::from_reader(buf.as_slice()).unwrap();
            assert_eq!(v, back);
        }
    }
}
