# Subscriptions — completed

- Per-record subscription tree: `SubscriptionsKey { id, snapshot }` packed key, CBOR `VARINT` value, `next_id` derived at boot — `crates/watch/src/engine.rs`.
- `WatchError::Storage(#[from] StorageError)` — `crates/watch/src/error.rs`.
- `Display` for `WatchEvent` — `crates/types/src/watch.rs`.
