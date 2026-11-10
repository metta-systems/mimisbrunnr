# Reconcile — open decisions

Design: [reconcile-design.md](reconcile-design.md) · Plan: [reconcile-implementation-plan.md](reconcile-implementation-plan.md)

## RECONCILE-D1 — Bounds on scan records and error entries

Two records reconcile writes have unbounded parts:

- `ScanRecord.params` (RECONCILE-DESIGN§1.3) is `bytes` with scan-kind-specific content — a disk id, a snapshot set, or the term ids of a rule rework, which can number thousands. `cursor_key` is capped at 256 bytes; `params` is not, and neither is the number of scans running at once.
- `ErrorEntry.detail` in the pool state's error log (POOL-DESIGN§1.2, RECONCILE-DESIGN§1.7) is `bytes` with no cap; the log keeps the newest 64.

Both are part of every checkpoint if [ENGINE-D2](engine-decisions.md) (c) carries them, and a worst case is needed either way. To decide: a cap on `params` (with large sets — rework term lists, snapshot sets — kept elsewhere and named by the record), the maximum number of concurrent scans, and a cap on `detail` (e.g. 64 bytes, a code and a short context).

## Future work

- Erasure-coding encode as a reconcile job (EC threshold trigger in RECONCILE-DESIGN§1.3).
- Format migration once a format is declared stable: migration scan driver, `fs_format_version` advance, `downgrade_log_ref` chain, per-structure migration rules, `mimir fsck --upgrade` (FORMAT-DESIGN§15.4, FORMAT-DESIGN§15.5).
