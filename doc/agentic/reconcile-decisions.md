# Reconcile — open decisions

Design: [reconcile-design.md](reconcile-design.md) · Plan: [reconcile-implementation-plan.md](reconcile-implementation-plan.md)

No open decisions recorded.

## Future work

- Erasure-coding encode as a reconcile job (EC threshold trigger in RECONCILE-DESIGN§1.3).
- Format migration once a format is declared stable: migration scan driver, `fs_format_version` advance, `downgrade_log_ref` chain, per-structure migration rules, `mimir fsck --upgrade` (FORMAT-DESIGN§15.4, FORMAT-DESIGN§15.5).
