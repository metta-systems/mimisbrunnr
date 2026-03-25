use {
    mimisbrunnr_types::{Query, SubscriptionId},
    roaring::RoaringBitmap,
};

use std::time::Duration;

/// What kinds of changes a subscription is interested in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeInterest(u32);

impl ChangeInterest {
    pub const TAG_ADDED: Self = Self(1 << 0);
    pub const TAG_REMOVED: Self = Self(1 << 1);
    pub const CONTENT_CHANGED: Self = Self(1 << 2);
    pub const CREATED: Self = Self(1 << 3);
    pub const DELETED: Self = Self(1 << 4);
    pub const ENTERED: Self = Self(1 << 5);
    pub const EXITED: Self = Self(1 << 6);
    pub const ALL: Self = Self(0x7F);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for ChangeInterest {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Lifecycle state of a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionState {
    /// Actively delivering events.
    Active,
    /// Agent is offline, cursor frozen.
    Dormant,
}

/// A persistent, named query watch.
#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: SubscriptionId,
    pub name: String,
    pub query: Query,
    pub interest: ChangeInterest,
    /// Where we've read up to in the oplog.
    pub cursor: u64,
    pub state: SubscriptionState,
    pub retention: Duration,
    /// Cached result set — the current matching objects.
    pub cached_result: RoaringBitmap,
}

impl Subscription {
    pub fn new(
        id: SubscriptionId,
        name: String,
        query: Query,
        interest: ChangeInterest,
        cursor: u64,
        initial_result: RoaringBitmap,
    ) -> Self {
        Self {
            id,
            name,
            query,
            interest,
            cursor,
            state: SubscriptionState::Active,
            retention: Duration::from_secs(7 * 24 * 3600), // 7 days default
            cached_result: initial_result,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state == SubscriptionState::Active
    }
}

#[cfg(test)]
mod tests {
    use {super::*, mimisbrunnr_types::TagId};

    #[test]
    fn change_interest_flags() {
        let interest = ChangeInterest::TAG_ADDED | ChangeInterest::CONTENT_CHANGED;
        assert!(interest.contains(ChangeInterest::TAG_ADDED));
        assert!(interest.contains(ChangeInterest::CONTENT_CHANGED));
        assert!(!interest.contains(ChangeInterest::DELETED));
    }

    #[test]
    fn subscription_creation() {
        let sub = Subscription::new(
            1,
            "test-watch".into(),
            Query::HasTag(TagId::new(1)),
            ChangeInterest::ALL,
            0,
            RoaringBitmap::new(),
        );
        assert!(sub.is_active());
        assert_eq!(sub.id, 1);
        assert_eq!(sub.name, "test-watch");
    }
}
