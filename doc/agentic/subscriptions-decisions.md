# Subscriptions — open decisions

Design: [subscriptions-design.md](subscriptions-design.md) · Plan: [subscriptions-implementation-plan.md](subscriptions-implementation-plan.md)

## SUBSCRIPTION-D1 — Continuous `mimir watch stream`

A streaming watch needs a long-running loop in the CLI. The workspace contract is sync-only (no async runtime); `std::thread` is allowed. With pull delivery (SUBSCRIPTIONS-DESIGN "Delivery") one candidate is a loop of `poll`, print, `ack`, waiting between empty polls; others are a helper thread driving the engine's `step` and delivering to the CLI, or waiting for a Metta-native event interface. Decide which, and how the loop waits (fixed interval, or a wake-up when the engine buffers an event). Blocks [SUBSCRIPTION5](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D6 — How events survive a restart or buffer overflow

Live events are evaluated into a bounded in-memory buffer and served by `poll`; `ack` persists the cursor (SUBSCRIPTIONS-DESIGN "Delivery"). When the buffer is lost (restart) or overflows, unacknowledged events must come from somewhere. Options:

- **(a) Membership at the cursor, replay from the journal.** The stored membership is the result as of the cursor; `ack` applies acknowledged events to it; a `poll` without a buffer replays journal entries from the cursor through the same evaluation. Live delivery and catch-up are one mechanism and nothing extra is stored. A subscriber that polls without acking holds journal history from its cursor, up to the per-pool retention cap (JOURNAL-DESIGN§1.6); past the cap it gets only the result-set diff, without the order of changes — which build tools depend on.
- **(b) Membership at the head, persisted pending events.** Membership follows every mutation; generated events are appended to a per-subscription event tree until acknowledged. Restart serves from that tree, in order, and holds no journal; the cost is an event write per reached subscription per mutation and unbounded growth for a subscriber that never acks (bounded by `retention`, SUBSCRIPTION-D2).
- **(c) Membership at the head, diff on loss.** Membership follows every mutation; a lost or overflowed buffer is replaced by a diff against a membership copy saved at each ack. No journal is held and nothing per event is written, but any restart or overflow loses order.

(a) and (b) keep order within their bounds; (c) never holds anything back. Shapes [SUBSCRIPTION8](subscriptions-implementation-plan.md), [SUBSCRIPTION6](subscriptions-implementation-plan.md), [SUBSCRIPTION1](subscriptions-implementation-plan.md) and SUBSCRIPTION-D2.

## SUBSCRIPTION-D2 — What `retention` means

SPECIFICATION-DESIGN§11.2 and §11.7 give a subscription `retention: Duration`: a dormant subscription whose consumer has not returned within it is garbage-collected (at boot and periodically). SUBSCRIPTIONS-DESIGN§1 and `crates/watch` use `retention: Retention` (`AtMostOnce` / `Bounded { max_events }` / `Unlimited`) for the pending-event queue instead, and nothing expires dormant subscriptions. Options: (a) keep both, adding an expiry duration and a last-active time to `SubscriptionRecord` (format change); (b) keep only the queue policy and drop expiry from the system design; (c) keep only expiry. The answer depends on SUBSCRIPTION-D6: under its option (a) a queue policy bounds only the in-memory event buffer and a dormant subscription costs the journal nothing beyond the per-pool retention cap (JOURNAL-DESIGN§1.6), so expiry is about record clutter, not log growth; under (b) the policy bounds the persisted event tree. Decide it together with that cap. Shapes [SUBSCRIPTION6](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D3 — Batch parameters in the record

SPECIFICATION-DESIGN§11.6 configures batching by debounce, maximum delay since the first held event, maximum batch size and coalescing; `SubscriptionRecord` holds only `debounce_ms`. Decide whether the other three become record fields (format change) or fixed engine defaults. Shapes [SUBSCRIPTION4](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D4 — `mimir watch` command surface

`bin/mimir` has `register <name> <query>`, `list`, `drain`, `unsubscribe`, `stream <name>`. SPECIFICATION-DESIGN Appendix A uses `mimir watch "<query>" --name <n>`, `list`, `catch-up <name>`, `stream "<query>" --format json`. Candidate additions: `--interest content-changed,created,…` and `--retention 30d` on creation; cursor lag (entries and age) in `list`; `catch-up` reporting the strategy used and event counts; `since <name>` summarising changes by kind; `remove`. Decide names and flags. Shapes [SUBSCRIPTION5](subscriptions-implementation-plan.md) and [SUBSCRIPTION6](subscriptions-implementation-plan.md).

## Future work

- **Richer events.** Report the assertion that caused `Entered` / `Exited`, old and new values for attribute changes (with an `ATTR_CHANGED` interest bit), old and new content hash and size for `ContentChanged`, and the initial assertions for `Created`. Interest groups `MUTATIONS` (tag, attribute, content changes) and `MEMBERSHIP` (entered, exited, created, deleted).
- **Ownership and delivery.** Record whether a subscription belongs to a process, a long-lived agent or the system, and deliver through an in-process callback, a Unix socket, an eventfd wakeup, polling, or accumulated batches.
