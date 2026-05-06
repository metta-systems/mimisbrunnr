# R1 fix plan — Tier 2: Spec-Violating But Live

Items here keep the system running but break one of: native bitmap COW,
value-hash collision rate, packed footprint, or recovery. None are blockers
for a smoke-test pool but each must land before the format can be considered
spec-compliant.

Prerequisite: Tier 1 (Cannot Ship) lands first. D1 + D2 give us real
`RootPointer` slots and bucket allocation; A1/A2/A4 give us the right
shape for the trees these items refine.

---

## C1. Resolve packed-codec `common_value_prefix` reconstruction asymmetry

**Spec:** IMPL §1.5.6 lines 406–417. Encoder elides leading bytes shared
by every value; reader reconstructs.

**Current state:** `crates/storage/src/btree/pack.rs:354-376`. Encoder
auto-detects the longest shared prefix (capped at `MAX_VALUE_PREFIX = 24`).
Single-entry runs make every byte "shared" → entire value elided. Reader
relies on a caller-supplied `prefix_template` whose bytes are not on disk.

### Steps

1. **Pin `common_value_prefix = 0` opt on the encoder side.**
   - File: `crates/storage/src/btree/pack.rs`.
   - Add `pub fn select_format_with_prefix<K, V>(entries, force_prefix: Option<u8>)`
     that lets callers pin the prefix length. `Some(0)` skips
     auto-detection. Pre-existing `select_format` calls
     `select_format_with_prefix(.., None)` (preserves current behaviour).
   - `BtreeRegion::write_full_packed` grows a `force_prefix_zero: bool`
     parameter; ChunkIndex etc. set it `true` until R6.

2. **Persist the prefix bytes inline (alternative to pinning).**
   - Spec amendment: extend `SortedRunKeyFormat` to carry the elided
     bytes inline (immediately after the field array, before per-key
     data). Net cost: ≤ 24 B per sorted run.
   - This is the *real* fix. Steps 1 above is the workaround until the
     spec amendment lands.
   - **Asks the user for spec sign-off before implementing.**

3. **Re-enable the ignored test.**
   - File: `crates/index/src/chunk_index.rs:703`. Drop the
     `#[ignore]` attribute on `region_overwrite_replaces_state` once
     either step 1 or step 2 is in place.

### Verification gate
- `region_overwrite_replaces_state` re-enabled and passing.
- New test in `pack.rs::tests`: encoder with `force_prefix_zero = true`
  produces `common_value_prefix == 0` even when every value happens to
  share leading bytes.

---

## C2. Variable-size values in the packed codec

**Spec:** IMPL §1.5.6 line 411 ("variable-shape values"). §7.1 LeafEntry
explicitly is variable-shape (inline body or spill_ref).

**Current state:** `crates/storage/src/btree/pack.rs:497-501`. Encoder
errors out if any two values have different lengths.

### Steps

1. **Per-entry length prefix in the value tail.**
   - File: `crates/storage/src/btree/pack.rs`.
   - New flag in `SortedRunKeyFormat`: `value_size_kind: u8` where
     `0 = fixed`, `1 = u8 length prefix`, `2 = u16 length prefix`,
     `3 = varint length prefix`. Reuses one of `_pad` / a free bit in
     `SortedRunHeader.flags` (e.g. `SORTED_RUN_FLAG_VARIABLE_VALUES`).
   - When variable, encoder emits `length_prefix(value.len()) || value_bytes`
     for each entry; decoder reads the prefix then the bytes. Prefix elision
     only applies to bytes shared at the **leading** offsets of every value
     after the length prefix.

2. **Encoder API.**
   - `encode_packed_run` ungated: `V: AsRef<[u8]>` already lets the
     caller hand in arbitrary lengths; current rejection at line 497 is
     the only thing standing in the way. Replace with: detect mixed
     lengths → switch to variable-length mode automatically; emit the
     length prefix per entry.

3. **Decoder API.**
   - `decode_packed_run` returns `Vec<(K, Vec<u8>)>` where each inner
     `Vec<u8>` carries exactly the value bytes the encoder ingested.
     Caller's `V: From<Vec<u8>>` impl rebuilds the typed value.

### Verification gate
- New test: insert mixed-length `Vec<u8>` values; round-trip preserves
  exact byte contents.
- ForwardIndex (Tier 2 A3.2) becomes implementable.

---

## C3. `FormatPromote` WAL op

**Spec:** IMPL §1.5.6 line 427. "Format upgrades are journalled as a
`FormatPromote` WAL op so recovery can reconstruct the in-memory sorted
run state."

**Current state:** No such variant in `crates/wal/src/op.rs`.

### Steps

1. **Add the WAL op variant.**
   - File: `crates/wal/src/op.rs` — `WalOpKind::FormatPromote = N` with
     payload `(BlockRef target, SortedRunKeyFormat new_format)`.
2. **Emit on append-on-flush format change.**
   - File: `crates/storage/src/btree.rs` `BtreeRegion::append_sorted_run`.
   - When the run being appended needs a wider format than the existing
     ones, emit `FormatPromote` into the WAL **before** the new run is
     written. (Recovery: on replay, find the latest `FormatPromote` for
     a region and apply it before reading subsequent sorted runs.)
3. **Replay path.**
   - File: `crates/engine/src/wal_proj.rs` `replay_wal_op`. Honour
     `FormatPromote` by re-decoding the affected sorted runs under the
     new format.

### Verification gate
- WAL replay test: insert keys triggering format promotion → checkpoint →
  drop → reopen → verify keys come back via the promoted format.

---

## A3. Native leaf encodings for ChunkIndex / ForwardIndex / TagIndex

Each migrates from CBOR-blob-in-sorted-run to the spec's exact byte
layout.

### A3.1. ChunkIndex `ChunkIndexLeafEntry` (56 B)

**Spec:** IMPL §9.3.

**Current state:** `crates/index/src/chunk_index.rs`. The 56 B struct
exists (line 73) but is unused on the wire. CBOR `ChunkEntrySerde`
drops the `length: u32` field.

#### Steps
1. Add `length: u32` to the in-memory `ChunkIndex` value (currently
   `(BlobRef, u32 ref_count)`); becomes `(BlobRef, u32 ref_count, u32 length)`.
2. Use the existing `ChunkIndexLeafEntry` struct as the wire form.
   `to_loaded_node` emits a sorted run keyed by `chunk_hash` with each
   value being `bytemuck::bytes_of(&ChunkIndexLeafEntry{..})` (40 B
   value tail because the 32 B hash is in the key).
3. Use the packed-key codec (4 × `u64` `bit_width=64, base=0` for the
   hash). Requires C1 (force_prefix_zero on the value to avoid the
   single-entry trap).

### A3.2. ForwardIndex `LeafEntry` + `ForwardOverflow` spill (variable)

**Spec:** IMPL §7.1 + §7.2.

**Current state:** `crates/index/src/forward_index.rs`. `LeafEntry`,
`PackedAssertion`, and serializer/parser exist (lines 246–354) but
unused. Wire form is CBOR `Vec<(Assertion, TagOrigin)>`.

#### Steps
1. **Promote `PackedAssertion.b` from `u32` to `u64`** (covers F2). Bumps
   `PackedAssertion` from 12 B to 16 B; matches IMPL §7.1 line 1656.
2. **Wire form is `LeafEntry` byte image.** `ForwardIndex::to_loaded_node`
   serializes each oid's assertions via `LeafEntry::serialise` (already
   in code).
3. **Inline-vs-spill discriminator.** When `total > 8`, write a
   `ForwardOverflow` 256 KiB region (positional, no sorted runs) and
   set the spill flag. `ForwardOverflow` sub-region kind:
   `BtreeKind::ForwardOverflow = 5` (already in spec).
4. **Variable-size value packing** depends on C2.

### A3.3. TagIndex `TagIndexLeafEntry` + `TagBitmapPage`

**Spec:** IMPL §8.1, §8.2, §8.3.

**Current state:** `crates/index/src/tag_index.rs`. `TagIndexLeafEntry`
struct exists (line 109) but unused. `TagBitmapPage` block envelope is
not implemented at all.

#### Steps
1. **`TagBitmapPage` 4 KiB block.** New file
   `crates/index/src/tag_bitmap_page.rs`:
   `BlockHeader (kind=TagBitmapPage) || roaring_bitmap_bytes (≤ 4060 B) || _pad || trailing CRC32C`.
   Each page COW'd independently. Allocator stamps the bucket
   `BucketDataType::Index`.
2. **Multi-page bitmap chains** for cardinalities > 4060 B. Trailing
   `next_page: BlockRef` slot. Inserts append a new tail page when
   the head fills; deletes mark sectors dirty and may trigger
   compaction (§12.6).
3. **Tag directory leaf** carries 48 B `TagIndexLeafEntry` per tag with
   `store_root: BlockRef` pointing at the head `TagBitmapPage` (or
   `SequencePage` / `RankedPage` for Ordered/Ranked stores).
4. **`SequencePage` / `RankedPage`** follow the same 4 KiB envelope
   pattern — defer to A3.4 / A3.5 once Simple stores work.

### Verification gate (A3)
- For each: native byte image written to disk, parseable by
  `bin/analyze` to confirm spec offsets.
- Round-trip tests rebuilt against the new wire form.
- Single-pool benchmark: 1 M chunks / 100 K objects / 10 K tags →
  expect at least 30% smaller index zone footprint than R1b's CBOR
  envelope (per spec §1.5.6 footnote).

---

## A5. SubscriptionEngine per-subscription B+ tree key

**Spec:** IMPL §10.2. `key = (SubscriptionId u64, snapshot u32) → SubscriptionRecord (CBOR)`.
One sorted-run entry per subscription.

**Current state:** `crates/watch/src/engine.rs`. Single sorted-run entry
`(snapshot=0u32, PersistedEngine_blob)`.

### Steps

1. **`PersistedSub` becomes the per-entry value.** Already a public
   struct (line 538 in user-revised). Single subscription's CBOR
   serialization is the value.
2. **Drop the `PersistedEngine` wrapper.** `next_id` is derivable from
   `max(subscriptions.keys()) + 1` at boot; persist nothing.
3. **`to_loaded_node` emits one entry per subscription.**
   `LoadedNode<(u64, u32), PersistedSub>`. Sorted by id.
4. **Mutating a single subscription** rewrites only its leaf entry —
   becomes possible once Tier 3 B1 lands `append_sorted_run` for live
   workloads.

### Verification gate
- Existing `watch_region_round_trip_*` tests rebuilt against the new
  key shape.
- New test: 1000 subscriptions → mutate sub 500 → verify only one leaf
  entry's bytes change on disk (post-Tier 3).

---

## F2. PackedAssertion `b: u64`

Folded into A3.2 step 1 above.

---

## F3. RangeIndex `(attr_id, value, oid, snapshot)` 4-field key

**Spec:** IMPL §9.2 line 1883. The oid component is part of the *key*,
not buried in a per-key bitmap value.

**Current state:** `crates/index/src/range_index.rs`. Key is
`RangeIndexKey { tag_id, norm_key, snapshot=0 }`. Oids live inside
`RangeIndexValue` (a roaring bitmap bytes blob).

### Steps

1. **Promote oid into the key.** New `RangeIndexKey { attr_id u32,
   value NormalisedKey, oid u64, snapshot u32 }`. Value becomes
   `BlockRef → TagBitmap` (16 B), reusing A3.3's `TagBitmapPage`
   machinery.
2. **`PackableKey` impl for the 4-field key.** All four fields are
   compressible: `attr_id` constant per leaf, `value` 16 B
   `NormalisedKey` (MSB-first packing), `oid` monotonic, `snapshot`
   typically constant.

### Verification gate
- Per-leaf packed footprint matches spec §9.2's "8–12 B per key" target.
- Range scan over `(attr_id, low_value, high_value)` returns the same
  set of oids as before.

---

## Suggested execution order (Tier 2)

1. **C1 then C2** (storage codec gaps; small, self-contained, unblock A3).
2. **C3** (FormatPromote WAL op).
3. **A3.1** (ChunkIndex native leaf — simplest fixed-size leaf).
4. **A3.3** (TagIndex + TagBitmapPage — biggest payoff, biggest scope).
5. **A3.2** (ForwardIndex — depends on C2 for variable-size values).
6. **A5 + F3** (per-key promotions in subscriptions and range index).
