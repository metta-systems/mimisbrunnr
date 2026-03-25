use mimisbrunnr_types::{HybridTimestamp, ObjectId, TagId};

/// The kind of operation recorded in the oplog (for subscriptions and sync).
#[derive(Debug, Clone, PartialEq)]
pub enum OpKind {
    CreateObject { oid: ObjectId },
    DeleteObject { oid: ObjectId },
    AddTag { oid: ObjectId, tag: TagId },
    RemoveTag { oid: ObjectId, tag: TagId },
    SetAttr { oid: ObjectId, tag: TagId },
    RemoveAttr { oid: ObjectId, tag: TagId },
    WriteBlob { oid: ObjectId },
}

/// A single oplog entry with a timestamp for ordering.
#[derive(Debug, Clone)]
pub struct OpLogEntry {
    pub timestamp: HybridTimestamp,
    pub lsn: u64,
    pub op: OpKind,
}

/// In-memory operation log for subscription catch-up.
pub struct OpLog {
    entries: Vec<OpLogEntry>,
}

impl OpLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: OpLogEntry) {
        self.entries.push(entry);
    }

    /// Get all entries since a given LSN (inclusive).
    pub fn since_lsn(&self, lsn: u64) -> &[OpLogEntry] {
        match self.entries.binary_search_by_key(&lsn, |e| e.lsn) {
            Ok(idx) => &self.entries[idx..],
            Err(idx) => &self.entries[idx..],
        }
    }

    /// Get the latest LSN, or 0 if empty.
    pub fn latest_lsn(&self) -> u64 {
        self.entries.last().map(|e| e.lsn).unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for OpLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(ms: u64) -> HybridTimestamp {
        HybridTimestamp::new(ms, 0, 0)
    }

    #[test]
    fn push_and_retrieve() {
        let mut log = OpLog::new();
        log.push(OpLogEntry {
            timestamp: ts(100),
            lsn: 1,
            op: OpKind::CreateObject {
                oid: ObjectId::new(0, 1),
            },
        });
        log.push(OpLogEntry {
            timestamp: ts(200),
            lsn: 2,
            op: OpKind::AddTag {
                oid: ObjectId::new(0, 1),
                tag: TagId::new(10),
            },
        });

        assert_eq!(log.len(), 2);
        assert_eq!(log.latest_lsn(), 2);
    }

    #[test]
    fn since_lsn() {
        let mut log = OpLog::new();
        for i in 1..=10 {
            log.push(OpLogEntry {
                timestamp: ts(i * 100),
                lsn: i,
                op: OpKind::CreateObject {
                    oid: ObjectId::new(0, i),
                },
            });
        }

        let since5 = log.since_lsn(5);
        assert_eq!(since5.len(), 6); // LSNs 5,6,7,8,9,10
        assert_eq!(since5[0].lsn, 5);
    }

    #[test]
    fn since_lsn_beyond_end() {
        let mut log = OpLog::new();
        log.push(OpLogEntry {
            timestamp: ts(100),
            lsn: 1,
            op: OpKind::CreateObject {
                oid: ObjectId::new(0, 1),
            },
        });

        let result = log.since_lsn(100);
        assert!(result.is_empty());
    }

    #[test]
    fn empty_log() {
        let log = OpLog::new();
        assert!(log.is_empty());
        assert_eq!(log.latest_lsn(), 0);
    }
}
