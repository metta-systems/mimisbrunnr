use mimisbrunnr_types::{HybridTimestamp, ObjectId, TagId};

/// A watch event delivered to a subscription.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchEvent {
    /// An object entered the subscription's result set (gained a matching tag).
    Entered {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
    /// An object exited the subscription's result set (lost a matching tag).
    Exited {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
    /// A tag was added to an object already in the result set.
    TagAdded {
        oid: ObjectId,
        tag: TagId,
        timestamp: HybridTimestamp,
    },
    /// A tag was removed from an object in the result set.
    TagRemoved {
        oid: ObjectId,
        tag: TagId,
        timestamp: HybridTimestamp,
    },
    /// An object in the result set had its content changed.
    ContentChanged {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
    /// An object in the result set was deleted.
    Deleted {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
    /// An object was created that matches the subscription query.
    Created {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
}

impl WatchEvent {
    pub fn object_id(&self) -> ObjectId {
        match self {
            Self::Entered { oid, .. }
            | Self::Exited { oid, .. }
            | Self::TagAdded { oid, .. }
            | Self::TagRemoved { oid, .. }
            | Self::ContentChanged { oid, .. }
            | Self::Deleted { oid, .. }
            | Self::Created { oid, .. } => *oid,
        }
    }

    pub fn timestamp(&self) -> HybridTimestamp {
        match self {
            Self::Entered { timestamp, .. }
            | Self::Exited { timestamp, .. }
            | Self::TagAdded { timestamp, .. }
            | Self::TagRemoved { timestamp, .. }
            | Self::ContentChanged { timestamp, .. }
            | Self::Deleted { timestamp, .. }
            | Self::Created { timestamp, .. } => *timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_accessors() {
        let ts = HybridTimestamp::new(1000, 0, 0);
        let oid = ObjectId::new(0, 42);
        let event = WatchEvent::Entered { oid, timestamp: ts };
        assert_eq!(event.object_id(), oid);
        assert_eq!(event.timestamp(), ts);
    }
}
