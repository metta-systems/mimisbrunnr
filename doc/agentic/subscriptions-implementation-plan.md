# Subscriptions — implementation plan

Design: [subscriptions-design.md](subscriptions-design.md) · Decisions: [subscriptions-decisions.md](subscriptions-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Persistence

- [ ] **SUBSCRIPTION1 — Externalize `cached_result`.** Replace the inline `cached_result: Vec<u8>` with posting containers (INDEX-DESIGN§3.2) stored out of line, and update SUBSCRIPTIONS-DESIGN§1. Prerequisite: [ALLOCATION1](allocation-implementation-plan.md).
- [ ] **SUBSCRIPTION2 — Per-subscription copy-on-write.** With append-on-flush, mutating one subscription appends a one-entry run. Prerequisite: [ENGINE1](engine-implementation-plan.md). Acceptance: 1 000 subscriptions, mutate one, only that entry's bytes change on disk.
- [ ] **SUBSCRIPTION7 — Positional, versioned `SubscriptionRecord`.** The record is written as SUBSCRIPTIONS-DESIGN§1 pins (array, schema version first, query through QUERY11). Prerequisite: [QUERY11](query-implementation-plan.md). Acceptance: a record round-trips through reopen; a record whose version is newer than the reader's fails with a defined `WatchError`; a record missing a trailing field reads with its documented default.

## Live behaviour

- [ ] **SUBSCRIPTION3 — Membership maintenance.** Engine mutation hooks re-evaluate affected subscriptions against the query executor and call `set_membership` (today no caller does; hooks only handle "watched tag added").
- [ ] **SUBSCRIPTION4 — Debounce flush.** `SubscriptionEngine::tick` flushes events whose debounce window elapsed (currently a no-op).
- [ ] **SUBSCRIPTION5 — `mimir watch stream`.** Prerequisite: SUBSCRIPTION-D1.
- [ ] **SUBSCRIPTION6 — Offline catch-up.** A replay loop delivers events from a subscription's cursor to the present (SPECIFICATION-DESIGN§11.5). Prerequisite: [ENGINE-D1](engine-decisions.md).
