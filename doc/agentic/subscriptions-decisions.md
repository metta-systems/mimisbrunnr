# Subscriptions — open decisions

Design: [subscriptions-design.md](subscriptions-design.md) · Plan: [subscriptions-implementation-plan.md](subscriptions-implementation-plan.md)

## SUBSCRIPTION-D2 — What `retention` means

SPECIFICATION-DESIGN§11.2 and §11.7 give a subscription `retention: Duration`: a dormant subscription whose consumer has not returned within it is garbage-collected (at boot and periodically). SUBSCRIPTIONS-DESIGN§1 and `crates/watch` use `retention: Retention` (`AtMostOnce` / `Bounded { max_events }` / `Unlimited`) for the pending-event queue instead, and nothing expires dormant subscriptions. Options: (a) keep both, adding an expiry duration and a last-active time to `SubscriptionRecord` (format change); (b) keep only the queue policy and drop expiry from the system design; (c) keep only expiry. With delivery from the journal (SUBSCRIPTIONS-DESIGN "Delivery"), a queue policy bounds only the in-memory event buffer, and a dormant subscription costs the journal nothing beyond the per-pool retention cap (JOURNAL-DESIGN§1.6) — past the cap it falls back to the diff — so expiry is about record clutter, not log growth; decide it together with that cap. Shapes [SUBSCRIPTION6](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D3 — Batch parameters in the record

SPECIFICATION-DESIGN§11.6 configures batching by debounce, maximum delay since the first held event, maximum batch size and coalescing; `SubscriptionRecord` holds only `debounce_ms`. Decide whether the other three become record fields (format change) or fixed engine defaults. Shapes [SUBSCRIPTION4](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D4 — `mimir watch` command surface

`bin/mimir` has `register <name> <query>`, `list`, `drain`, `unsubscribe`, `stream <name>`. SPECIFICATION-DESIGN Appendix A uses `mimir watch "<query>" --name <n>`, `list`, `catch-up <name>`, `stream "<query>" --format json`. Candidate additions: `--interest content-changed,created,…` and `--retention 30d` on creation; cursor lag (entries and age) in `list`; `catch-up` reporting the strategy used and event counts; `since <name>` summarising changes by kind; `remove`. Decide names and flags. Shapes [SUBSCRIPTION5](subscriptions-implementation-plan.md) and [SUBSCRIPTION6](subscriptions-implementation-plan.md).

## Future work

- **Richer events.** Report the assertion that caused `Entered` / `Exited`, old and new values for attribute changes (with an `ATTR_CHANGED` interest bit), old and new content hash and size for `ContentChanged`, and the initial assertions for `Created`. Interest groups `MUTATIONS` (tag, attribute, content changes) and `MEMBERSHIP` (entered, exited, created, deleted).
- **Ownership and delivery.** Record whether a subscription belongs to a process, a long-lived agent or the system, and deliver through an in-process callback, a Unix socket, an eventfd wakeup, polling, or accumulated batches.
