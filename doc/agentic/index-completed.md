# Indices — completed

- KV index native extendible hash: `KvDirectoryBlock`, `KvBucketBlock`, bitmaps in `TagBitmapPage`s within one region — `crates/index/src/kv_directory.rs`, `kv_bucket.rs`, `kv_index.rs`, test `kv_index_on_disk_uses_native_extendible_hash_layout`.
- Chunk index native 32 B key + 24 B value tail through the packed codec — `crates/index/src/chunk_index.rs`.
- Forward index native `LeafEntry` packed `VARINT` leaf with `ForwardOverflow` region chains and `CorruptOverflowChain` / `OverflowAreaExhausted` errors — `crates/index/src/forward_index.rs`.
- `PackedAssertion` is 16 B with `b: u64` — `crates/index/src/forward_index.rs`.
- Tag index Simple stores: 40 B directory value tails and chained 4 KiB `TagBitmapPage`s — `crates/index/src/tag_index.rs`, `tag_bitmap_page.rs`.
- `ForwardIndex::iter` public — `crates/index/src/forward_index.rs`.
- INDEX9: `Value` encodes as INDEX-DESIGN§1.1 positional CBOR through `Value::encode` / `decode`, its serde form is the `(tag, payload)` pair; positional-CBOR helpers `CborWriter`, `CborReader`, `RecordFields` — `crates/types/src/cbor.rs`, `value.rs`, tests `every_variant_encodes_to_its_pinned_bytes`, `nan_is_rejected_both_ways`, `the_unassigned_tag_5_is_rejected`, `serde_writes_the_pinned_pair`, `records_default_missing_trailing_fields_and_reject_newer_versions`.
