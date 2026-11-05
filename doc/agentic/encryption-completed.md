# Encryption — completed

- Sector padding for XTS / HCTR2 in the transform pipeline — `crates/transform`.
- `BLOCK_FLAG_ENCRYPTED` and `WAL_ENTRY_FLAG_ENCRYPTED` recognised and rejected until ciphers exist — `crates/storage`, `crates/wal`.
