use mimisbrunnr_types::Query;

use crate::StorageTier;

/// Semantic placement rules that bind tag queries to physical topology.
///
/// These rules drive where blobs are stored based on their metadata,
/// replacing the per-dataset approach of traditional filesystems.
#[derive(Debug, Clone)]
pub enum PlacementRule {
    /// Force objects matching `query` onto a specific tier.
    Pin {
        query: Query,
        tier: StorageTier,
    },
    /// Prefer placing matching objects on a tier (soft constraint).
    Prefer {
        query: Query,
        tier: StorageTier,
        priority: u8,
    },
    /// Replicate matching objects across multiple disks.
    Replicate {
        query: Query,
        min_replicas: u8,
        across_disks: bool,
    },
    /// Co-locate matching objects on the same disk for locality.
    Colocate {
        query: Query,
    },
    /// Automatic tiering based on access age.
    AutoTier {
        hot_threshold_days: u32,
        warm_threshold_days: u32,
        cold_after: u32,
    },
}

impl PlacementRule {
    /// Get the query associated with this rule, if any.
    pub fn query(&self) -> Option<&Query> {
        match self {
            Self::Pin { query, .. }
            | Self::Prefer { query, .. }
            | Self::Replicate { query, .. }
            | Self::Colocate { query } => Some(query),
            Self::AutoTier { .. } => None,
        }
    }

    /// Get the target tier for this rule, if it specifies one.
    pub fn target_tier(&self) -> Option<StorageTier> {
        match self {
            Self::Pin { tier, .. } | Self::Prefer { tier, .. } => Some(*tier),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr_types::TagId;

    #[test]
    fn pin_rule() {
        let rule = PlacementRule::Pin {
            query: Query::HasTag(TagId::new(1)),
            tier: StorageTier::Hot,
        };
        assert_eq!(rule.target_tier(), Some(StorageTier::Hot));
        assert!(rule.query().is_some());
    }

    #[test]
    fn prefer_rule() {
        let rule = PlacementRule::Prefer {
            query: Query::HasTag(TagId::new(2)),
            tier: StorageTier::Cold,
            priority: 5,
        };
        assert_eq!(rule.target_tier(), Some(StorageTier::Cold));
    }

    #[test]
    fn replicate_rule() {
        let rule = PlacementRule::Replicate {
            query: Query::HasTag(TagId::new(3)),
            min_replicas: 3,
            across_disks: true,
        };
        assert!(rule.target_tier().is_none());
        assert!(rule.query().is_some());
    }

    #[test]
    fn auto_tier_rule() {
        let rule = PlacementRule::AutoTier {
            hot_threshold_days: 7,
            warm_threshold_days: 30,
            cold_after: 90,
        };
        assert!(rule.target_tier().is_none());
        assert!(rule.query().is_none());
    }
}
