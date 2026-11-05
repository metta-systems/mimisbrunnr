# Engine — open decisions

Design: [engine-design.md](engine-design.md) · Plan: [engine-implementation-plan.md](engine-implementation-plan.md)

## ENGINE-D1 — Oplog persistence

The oplog lives in the legacy `MIXI` blob and the spec defines no oplog structure. Options: (a) rebuild it from the WAL at boot and persist nothing (preferred so far); (b) a dedicated B+ tree keyed by `oplog_seq`, which needs a new `BtreeKind` slot in FORMAT-DESIGN§1.3. The choice also fixes how far back subscriptions can catch up ([SUBSCRIPTION4](subscriptions-implementation-plan.md)) and how cluster sync reads history ([cluster-design.md](cluster-design.md)). Blocks [ENGINE6](engine-implementation-plan.md).

## Future work

- None recorded.
