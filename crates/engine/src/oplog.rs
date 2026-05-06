//! In-memory operation log + engine-level [`OpKind`] enum (DESIGN §15).
//!
//! `OpKind` is the **engine-level** mutation set. It is distinct from
//! [`mimisbrunnr_wal::WalOpKind`] (IMPL §3.3), which has the full universe of
//! 32 byte-pinned WAL discriminants (object lifecycle, chunked-flow, bucket
//! lifecycle, backpointers, snapshots, reconcile, …). Each engine op projects
//! to one or more WAL ops; the mapping lives in
//! [`crate::wal_proj::project_op`].

use std::collections::VecDeque;

use mimisbrunnr_types::{HybridTimestamp, ObjectId, TagId, Value};
use serde::{Deserialize, Serialize};

/// One mutation in the engine-level log. Order matches DESIGN §15 table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OpKind {
    /// Allocate a fresh `ObjectId`.
    CreateObject { oid: ObjectId },
    /// Mark an object tombstoned (DESIGN §7).
    DeleteObject { oid: ObjectId },
    /// Add a tag with `Direct` origin; engine fans out implications as
    /// additional `Materialized` insertions (which are *not* logged again —
    /// they're recoverable from the original direct edge plus the ontology).
    AddTag { oid: ObjectId, tag: TagId },
    /// Remove a `Direct` tag and its derived materialised tags.
    RemoveTag { oid: ObjectId, tag: TagId },
    /// Set / overwrite a `(key, value)` attribute.
    SetAttr {
        oid: ObjectId,
        key: TagId,
        value: Value,
    },
    /// Remove an attribute by `(key, value_hash)`.
    RemoveAttr {
        oid: ObjectId,
        key: TagId,
        value_hash: u64,
    },
    /// Add a `(predicate, target)` relation.
    AddRelation {
        oid: ObjectId,
        predicate: TagId,
        target: ObjectId,
    },
    /// Remove a `(predicate, target)` relation.
    RemoveRelation {
        oid: ObjectId,
        predicate: TagId,
        target: ObjectId,
    },
    /// Non-chunked blob write — content metadata only; the blob bytes go to
    /// the blob zone.
    WriteBlob {
        oid: ObjectId,
        content_hash: [u8; 32],
        size: u64,
    },
}

/// One entry in the in-memory oplog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpLogEntry {
    pub timestamp: HybridTimestamp,
    pub lsn: u64,
    pub op: OpKind,
}

/// Default capacity of the in-memory oplog ring (DESIGN §15: "recent" ops for
/// subscriptions / sync). When the ring is full, oldest entries are evicted.
pub const DEFAULT_OPLOG_CAPACITY: usize = 4096;

/// In-memory bounded ring of recent [`OpLogEntry`]s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpLog {
    entries: VecDeque<OpLogEntry>,
    max_len: usize,
}

impl OpLog {
    /// New oplog with the given ring capacity. `0` is treated as
    /// [`DEFAULT_OPLOG_CAPACITY`].
    pub fn new(capacity: usize) -> Self {
        let max_len = if capacity == 0 {
            DEFAULT_OPLOG_CAPACITY
        } else {
            capacity
        };
        Self {
            entries: VecDeque::with_capacity(max_len.min(1024)),
            max_len,
        }
    }

    /// Push a new op, evicting the oldest entry when full.
    pub fn record(&mut self, op: OpKind, lsn: u64, ts: HybridTimestamp) {
        while self.entries.len() >= self.max_len {
            self.entries.pop_front();
        }
        self.entries.push_back(OpLogEntry {
            timestamp: ts,
            lsn,
            op,
        });
    }

    /// Iterator over entries with `lsn >= start_lsn`.
    pub fn since(&self, start_lsn: u64) -> impl Iterator<Item = &OpLogEntry> {
        self.entries.iter().filter(move |e| e.lsn >= start_lsn)
    }

    /// Number of entries currently held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Configured ring capacity.
    pub fn capacity(&self) -> usize {
        self.max_len
    }

    /// CBOR-encode for persistence. TODO(rewrite-phase-N): replace with the
    /// IMPL §10 oplog btree once the structured persistence layer lands.
    pub fn serialise(&self) -> Result<Vec<u8>, crate::EngineError> {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(self, &mut buf)?;
        Ok(buf)
    }

    /// CBOR-decode. TODO(rewrite-phase-N) as above.
    pub fn deserialise(bytes: &[u8]) -> Result<Self, crate::EngineError> {
        let oplog: Self = ciborium::de::from_reader(bytes)?;
        Ok(oplog)
    }
}

impl Default for OpLog {
    fn default() -> Self {
        Self::new(DEFAULT_OPLOG_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(ns: i64) -> HybridTimestamp {
        HybridTimestamp::new(ns, 0, 0)
    }

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    #[test]
    fn record_and_since() {
        let mut log = OpLog::new(0);
        for i in 1..=5u64 {
            log.record(
                OpKind::CreateObject { oid: oid(i) },
                i,
                ts(100 + i as i64),
            );
        }
        let collected: Vec<u64> = log.since(0).map(|e| e.lsn).collect();
        assert_eq!(collected, vec![1, 2, 3, 4, 5]);
        let from3: Vec<u64> = log.since(3).map(|e| e.lsn).collect();
        assert_eq!(from3, vec![3, 4, 5]);
    }

    #[test]
    fn evicts_oldest_when_full() {
        let mut log = OpLog::new(3);
        for i in 1..=5u64 {
            log.record(OpKind::CreateObject { oid: oid(i) }, i, ts(i as i64));
        }
        assert_eq!(log.len(), 3);
        let lsns: Vec<u64> = log.since(0).map(|e| e.lsn).collect();
        assert_eq!(lsns, vec![3, 4, 5]);
    }

    #[test]
    fn cbor_round_trip() {
        let mut log = OpLog::new(8);
        log.record(
            OpKind::AddTag {
                oid: oid(1),
                tag: TagId::new(5),
            },
            42,
            ts(1_000_000),
        );
        let bytes = log.serialise().unwrap();
        let back = OpLog::deserialise(&bytes).unwrap();
        assert_eq!(back.len(), 1);
        let entries: Vec<_> = back.since(0).collect();
        assert_eq!(entries[0].lsn, 42);
    }
}
