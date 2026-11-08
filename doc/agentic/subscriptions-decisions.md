# Subscriptions — open decisions

Design: [subscriptions-design.md](subscriptions-design.md) · Plan: [subscriptions-implementation-plan.md](subscriptions-implementation-plan.md)

## SUBSCRIPTION-D1 — Continuous `mimir watch stream`

A streaming watch needs a long-running loop in the CLI. The workspace contract is sync-only (no async runtime); `std::thread` is allowed. With pull delivery (SUBSCRIPTIONS-DESIGN "Delivery") one candidate is a loop of `poll`, print, `ack`, waiting between empty polls; others are a helper thread driving the engine's `step` and delivering to the CLI, or waiting for a Metta-native event interface. Decide which, and how the loop waits (fixed interval, or a wake-up when the engine buffers an event). Blocks [SUBSCRIPTION5](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D2 — What `retention` means

One reading of `retention` (SPECIFICATION-DESIGN§11.7) is an expiry duration: a dormant subscription whose consumer has not returned within it is garbage-collected (at boot and periodically). SUBSCRIPTIONS-DESIGN§1 and `crates/watch` use `retention: Retention` (`AtMostOnce` / `Bounded { max_events }` / `Unlimited`) for the pending-event queue instead, and nothing expires dormant subscriptions. Options: (a) keep both, adding an expiry duration and a last-active time to `SubscriptionRecord` (format change); (b) keep only the queue policy and drop expiry from the system design; (c) keep only expiry. With persisted pending events (SUBSCRIPTIONS-DESIGN "Delivery") the queue policy is the bound past which a subscription overflows to the result-set diff, and a dormant subscription costs only its own bounded events, never journal history; expiry would then only clear records nobody returns to. Shapes [SUBSCRIPTION6](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D3 — Batch parameters in the record

SPECIFICATION-DESIGN§11.6 configures batching by debounce, maximum delay since the first held event, maximum batch size and coalescing; `SubscriptionRecord` holds only `debounce_ms`. Decide whether the other three become record fields (format change) or fixed engine defaults. Shapes [SUBSCRIPTION4](subscriptions-implementation-plan.md).

## SUBSCRIPTION-D4 — `mimir watch` command surface

`bin/mimir` has `register <name> <query>`, `list`, `drain`, `unsubscribe`, `stream <name>`. SPECIFICATION-DESIGN Appendix A uses `mimir watch "<query>" --name <n>`, `list`, `catch-up <name>`, `stream "<query>" --format json`. Candidate additions: `--interest content-changed,created,…` and `--retention 30d` on creation; cursor lag (entries and age) in `list`; `catch-up` reporting the strategy used and event counts; `since <name>` summarising changes by kind; `remove`. Decide names and flags. Shapes [SUBSCRIPTION5](subscriptions-implementation-plan.md) and [SUBSCRIPTION6](subscriptions-implementation-plan.md).

## Future work

- **Richer events.** Report the assertion that caused `Entered` / `Exited`, old and new values for attribute changes (with an `ATTR_CHANGED` interest bit), old and new content hash and size for `ContentChanged`, and the initial assertions for `Created`. Interest groups `MUTATIONS` (tag, attribute, content changes) and `MEMBERSHIP` (entered, exited, created, deleted).
- **Ownership and delivery.** Record whether a subscription belongs to a process, a long-lived agent or the system, and deliver through an in-process callback, a Unix socket, an eventfd wakeup, polling, or accumulated batches.
