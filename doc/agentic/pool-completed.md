# Pool management — completed

- Placement rules persisted in a B+ tree region — `crates/pool/src/placement.rs`.
- Disk descriptors persisted per disk in the `disks_overflow` region — `crates/pool/src/pool.rs`.
- `StorageTier::name` / `as_str` (and the other disk enums) — `crates/types/src/disk.rs`.
- `PoolManager::add_disk` records the new disk in `PoolConfig` — `crates/pool/src/tier.rs`.
- `brunnr create`, `status`, `add-disk`, `remove-disk` — `bin/brunnr`.
