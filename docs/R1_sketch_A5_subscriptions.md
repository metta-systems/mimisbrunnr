# R1c-A5 sketch — Subscriptions per-record B+ tree

**Status:** proposal, awaiting sign-off.

**Reference:** `docs/R1_fix02_spec_violating.md` §A5; spec §10.2.

## Feature being fixed

Today the `SubscriptionEngine` persists every subscription as a single
fat CBOR blob — one sorted-run entry keyed by `snapshot=0u32`, value =
the entire `PersistedEngine { next_id, Vec<PersistedSub> }`. Mutating
one subscription's cursor (or membership bitmap, or retention) forces
us to re-serialise every subscription in the pool and rewrite the
whole 256 KiB region.

The spec (IMPL §10.2) wants a true B+ tree of subscriptions, one entry
per subscription:

```
SubscriptionsRoot:
  §1.5 B+ tree, key = (SubscriptionId u64, snapshot u32) → SubscriptionRecord (variable, CBOR)
```

This brings two properties:
- **Per-sub COW.** One subscription's bytes change without disturbing
  the others (delivered once Tier 3 B1 wires append-on-flush).
- **Cardinality scaling.** Pool can grow to thousands of subscriptions
  without re-serialising all of them on every commit.

The §10.2 spec also says the `cached_result` roaring bitmap lives in
an externalized `TagBitmap` (§8.2) referenced from the record by
`BlockRef`. We discuss whether to land that part now or defer (see S1
below).

## Scope of change

**Inside scope:**
- Storage format: switch from single-entry `(snapshot=0, PersistedEngine)`
  to per-subscription `((SubscriptionId, snapshot=0), PersistedSub)`.
- Key encoding: add `PackableKey` impl for the 2-field key (mirrors
  what TagIndex / ForwardIndex now do).
- Value encoding: VARINT-mode packed run carrying per-subscription
  CBOR bytes (we already have the codec from C2).
- `next_id` persistence: derive on boot from `max(subscription_ids) + 1`
  — drop the standalone field. Matches the spec's pattern for
  `next_oid_local` / `next_tag_id` (D4).
- `SubscriptionEngine::{flush_to_region, load_from_region}` get the
  same shape as today (single `device, offset` args) — no external
  area needed if we keep bitmaps inline (S1 below).
- Migration: one-way format break. Pre-A5 pools recreate.

**Outside scope (deferred):**
- **Bitmap externalization to a `TagBitmap` `BlockRef`** — full spec
  compliance for `cached_result`. Gated on Tier 3 D3 sub-bucket
  allocation so per-sub bitmap pages don't compete with the
  fixed-offset bitmap-area cap. Tracked as a follow-up below.
- **Snapshot-aware reads.** `snapshot` field always `0` until R6.
- **Append-on-flush.** Each commit still rewrites the directory region
  via `write_full_packed_force_varint`. Per-sub COW becomes real once
  Tier 3 B1 lands.

## Affected sections (IMPL.md)

- **§10.2** — clarify the per-record key + value byte layout.
- **§16 footprint table** (if present) — adjust the subscriptions row.
- **§13** in-memory mirror table — note `SubscriptionEngine` now
  derives `next_id` from `max(subscriptions.keys()) + 1` at boot.

## Decisions for sign-off

### S1. Bitmap externalization — defer or land now?

The spec mandates `cached_result` lives in a §8.2 `TagBitmap` linked
from the record by `BlockRef`. The current code carries the bitmap's
byte image inline in the CBOR `PersistedSub.cached_result: Vec<u8>`.

- **(a) Defer.** Keep the bitmap inline in CBOR for A5. Spec-violating
  in the strict sense; pragmatic because per-sub bitmaps are usually
  small (a few dozen oids in practice). Externalization waits for
  Tier 3 D3 sub-bucket allocation so per-sub `TagBitmapPage`s can be
  allocated cheaply without competing with the tag-bitmap-area cap.
  Track as a follow-up TODO. **(recommended)**
- **(b) Land now.** Add a per-pool *subscription bitmap area* at a
  fixed engine offset (after the forward-overflow area at 16 MiB; cap
  e.g. 256 pages = 1 MiB) and write each sub's bitmap into its own
  chain. Bumps `FMT_INDEX_ZONE_SIZE` again. Compatible with the
  existing TagBitmapPage code.
- **(c) Share the tag-bitmap area.** One allocator for both tags and
  subs. Saves zone footprint but couples the two consumers.

> **Default:** (a). Spec-compliance lift is small (the field is still
> a `Vec<u8>` byte image internally; the difference is whether it's
> inline in the record vs. linked by `BlockRef`).

### S2. `next_id` persistence

- **(a) Derive on boot** from `max(subscription_ids) + 1`; drop the
  field entirely. Matches D4's pattern for other counters.
  **(recommended)**
- **(b) Persist** in a sentinel entry (`SubscriptionId::MAX`).
- **(c) Persist** in a small fixed header before the directory region.

> **Default:** (a). Even after deletes, `next_id` only needs to be
> *greater than* every previously-issued id, and `max + 1` over the
> live set is a tight lower bound. Reuse after delete is fine —
> subscriptions are identified by id within a session, not across
> deletes.

### S3. Key shape — 2-field `(SubscriptionId u64, snapshot u32)`

Mirrors TagIndex / ForwardIndex / Range. Both fields packed via
`PackableKey`. `snapshot=0` until R6.

> **No decision needed.** Matches the spec verbatim. Confirm.

### S4. Format version bump?

Following the established pattern: **no** — we're still designing for
format_version = 1 and accept one-way migration breaks across R1c
landings.

> **No decision needed unless the user wants a bump.**

### S5. Engine flush signature

Under S1=(a), the `flush_to_region(device, offset)` signature stays
the same. Under S1=(b), it grows to
`flush_to_region(device, dir_offset, sub_bitmap_area_offset,
sub_bitmap_area_cap_pages)`.

> Falls out of S1.

### S6. Migration

One-way format break. Pre-A5 pools won't load under A5. Same
recreate-the-pool policy as A3.1/A3.2/A3.3.

> **No decision needed.** Confirm.

## Proposed §10.2 spec amendment (S1=(a) path)

```rust
// IMPL §10.2 — SubscriptionsRoot

// Directory: §1.5 B+ tree region of BtreeKind::Subscriptions.
// One leaf entry per subscription:
//
//   key   = (SubscriptionId: u64, snapshot: u32)
//             — 2-field packed key (§1.5.6); snapshot=0 until R6.
//   value = CBOR(SubscriptionRecord)
//             — variable-length; persisted via VALUE_SIZE_KIND_VARINT.

#[derive(Serialize, Deserialize)]
struct SubscriptionRecord {                  // CBOR; ~100–400 B typical
    name:           String,                  //   human label
    query:          Query,                   //   subscription's predicate AST
    interest:       ChangeInterest,          //   add/remove/update mask
    cursor:         u64,                     //   last delivered WAL LSN
    state:          SubscriptionState,       //   Active/Paused/Cancelled
    retention:      Retention,               //   Unlimited / AtMostOnce / Bounded{n}
    debounce_ms:    u32,                     //   coalesce window
    cached_result:  Vec<u8>,                 //   roaring bitmap byte image
                                             //   (R1c-A5 inline — see TODO
                                             //   for D3 externalization to
                                             //   §8.2 TagBitmap by BlockRef)
}
```

`next_id` is **not persisted**: it's re-derived at boot from
`max(subscription_id) + 1` over the loaded set. Empty pool → next_id = 1.

> TODO(post-D3): replace `cached_result: Vec<u8>` with
> `cached_result_root: BlockRef` pointing at a §8.2 `TagBitmapPage`
> chain. This is the spec's mandated form; deferred only to align with
> D3 sub-bucket allocation.

## Implementation plan (post sign-off)

1. **Storage: nothing new.** Reuse `write_full_packed_force_varint`
   from A3.2.
2. **Types in watch crate.**
   - Rename `PersistedSub` → `SubscriptionRecord` (or keep current
     name, doc-update only).
   - Drop `PersistedEngine` (and its `next_id` field).
   - Add `SubscriptionsKey { id: u64, snapshot: u32 }` with
     `PackableKey` impl (2 fields, both unsigned).
   - `SubscriptionsValue(Vec<u8>)` — byte-tail wrapper with
     `AsRef<[u8]>` + `From<Vec<u8>>` (mirrors ForwardIndexValue).
3. **Rewrite `to_loaded_node` / `from_loaded_node`.** One entry per
   subscription, sorted by id.
4. **`flush_to_region` / `load_from_region`.** Switch to
   `write_full_packed_force_varint` / `read_packed`. Probe-zero check
   for empty regions stays.
5. **Boot:** derive `next_id` from `max(keys) + 1` after load.
6. **Tests.**
   - `subscriptions_region_round_trip_empty_returns_default`.
   - `subscriptions_region_round_trip_single_sub`.
   - `subscriptions_region_round_trip_many_subs` (e.g. 50).
   - `subscriptions_region_uses_varint_codec` (inspect on-disk run
     header flags).
   - `subscription_next_id_derived_from_max_plus_one`.
   - `subscription_region_kind_mismatch_detected`.
7. **Engine integration unchanged** — `flush_to_region` signature
   stays under S1=(a).

## Verification gate

- `cargo test -p mimisbrunnr-watch` green.
- `cargo test --workspace` green.
- `cargo clippy --workspace --all-targets` clean.
- Round-trip test: 50 subscriptions written, single sub mutated,
  verify (post-Tier 3 B1) only that sub's bytes change on disk. For
  R1c-A5 (full rewrite), assert only that the per-sub on-disk image
  is byte-identical between flushes when nothing changes.

## Sign-off requested

Pick a letter / yes-no for each:

- **S1** Bitmap externalization: **(a)** defer / **(b)** land now / **(c)** share tag-bitmap area.
- **S2** `next_id`: **(a)** derive / **(b)** sentinel / **(c)** fixed header.
- **S3** Key shape `(u64, u32)` — confirm.
- **S4** Format version bump — confirm "no".
- **S5** Engine flush signature — falls out of S1.
- **S6** One-way format break — confirm.
