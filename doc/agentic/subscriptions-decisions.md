# Subscriptions — open decisions

Design: [subscriptions-design.md](subscriptions-design.md) · Plan: [subscriptions-implementation-plan.md](subscriptions-implementation-plan.md)

## SUBSCRIPTION-D1 — Continuous `mimir watch stream`

A streaming watch needs a long-running loop or background thread in the CLI. The workspace contract is sync-only (no async runtime); `std::thread` is allowed. Decide whether streaming uses a blocking poll loop, a thread, or waits for a Metta-native event interface. Blocks [SUBSCRIPTION5](subscriptions-implementation-plan.md).

## Future work

- None recorded.
