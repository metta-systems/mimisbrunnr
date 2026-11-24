# Subscriptions — open decisions

Design: [subscriptions-design.md](subscriptions-design.md) · Plan: [subscriptions-implementation-plan.md](subscriptions-implementation-plan.md)

## SUBSCRIPTION-D1 — Continuous `mimir watch stream`

A streaming watch needs a long-running loop in the CLI. The workspace contract is sync-only (no async runtime); `std::thread` is allowed. With pull delivery (SUBSCRIPTIONS-DESIGN "Delivery") one candidate is a loop of `poll`, print, `ack`, waiting between empty polls; others are a helper thread driving the engine's `step` and delivering to the CLI, or waiting for a Metta-native event interface. Decide which, and how the loop waits (fixed interval, or a wake-up when the engine buffers an event). Blocks [SUBSCRIPTION5](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D5 — Encoding of the event bound; activity writes

The record's `retention` bounds the pending-event count (SUBSCRIPTIONS-DESIGN "Delivery"); `crates/watch` still has `Retention { AtMostOnce, Bounded { max_events }, Unlimited }` with ring-buffer semantics (oldest dropped) instead of overflow. To decide:

- **Bound:** (a) `max_events: uint` replacing `retention` (bulk entries count their objects, as `pending_count` does), at least 1, default 1024; `AtMostOnce` is a bound of 1 and `Unlimited` goes; (b) keep a `Retention` variant array (`[0, max_events]`, room for other bound kinds).
- **Activity writes:** `last_active_ns` is set by `ack` (already a record write) and by `poll`, which otherwise writes nothing. (a) `poll` rewrites the record at most once per day per subscription, so expiry is accurate to a day; (b) on every poll that returns nothing new. A consumer whose query never matches never acknowledges, so `ack` alone would expire live consumers.

Recommendation: (a), (a). Shapes [SPEC25](specification-implementation-plan.md), [SUBSCRIPTION8](subscriptions-implementation-plan.md) and [SUBSCRIPTION9](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D3 — Batch parameters in the record

SPECIFICATION-DESIGN§11.6 configures batching by debounce, maximum delay since the first held event, maximum batch size and coalescing; `SubscriptionRecord` holds only `debounce_ms`. Decide whether the other three become record fields (format change) or fixed engine defaults. Shapes [SUBSCRIPTION4](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D4 — `mimir watch` command surface

`bin/mimir` has `register <name> <query>`, `list`, `drain`, `unsubscribe`, `stream <name>`. SPECIFICATION-DESIGN Appendix A uses `mimir watch "<query>" --name <n>`, `list`, `catch-up <name>`, `stream "<query>" --format json`. Candidate additions: `--interest content-changed,created,…` and `--max-events <n>` and `--expire-after 90d` on creation; cursor lag (entries and age) in `list`; `catch-up` reporting the strategy used and event counts; `since <name>` summarising changes by kind; `remove`. Decide names and flags. Shapes [SUBSCRIPTION5](subscriptions-implementation-plan.md) and [SUBSCRIPTION6](subscriptions-implementation-plan.md).

## Future work

- **Richer events.** Report the assertion that caused `Entered` / `Exited`, old and new values for attribute changes (with an `ATTR_CHANGED` interest bit), old and new content hash and size for `ContentChanged`, and the initial assertions for `Created`. Interest groups `MUTATIONS` (tag, attribute, content changes) and `MEMBERSHIP` (entered, exited, created, deleted).
- **Ownership and delivery.** Record whether a subscription belongs to a process, a long-lived agent or the system, and deliver through an in-process callback, a Unix socket, an eventfd wakeup, polling, or accumulated batches.
