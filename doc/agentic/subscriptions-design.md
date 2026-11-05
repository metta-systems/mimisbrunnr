# Subscriptions — design

Scope: `crates/watch` (`SubscriptionEngine`, `Subscription`), engine mutation hooks, `mimir watch` commands.
Governing spec: SPECIFICATION-DESIGN§11, SUBSCRIPTIONS-DESIGN§1.

Plan: [subscriptions-implementation-plan.md](subscriptions-implementation-plan.md) · Open decisions: [subscriptions-decisions.md](subscriptions-decisions.md) · Completed: [subscriptions-completed.md](subscriptions-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

A subscription is a saved query plus a cursor; every mutation updates matching subscriptions' membership, and a subscriber that was offline catches up from its cursor.

## Selected design

- **Per-record tree.** One sorted-run entry per subscription: key `(SubscriptionId: u64, snapshot: u32)` through the packed-key codec, value the CBOR `SubscriptionRecord` under `VARINT` value size.
- **`next_id` is derived at boot** as `max(loaded ids)` (0 when empty); the allocator pre-bumps, so `next_id` equals the last assigned id.
- **`cached_result`** is inline in the record until sub-bucket allocation exists; it then moves to a §8.2 `TagBitmapPage` chain referenced by `BlockRef`, so a cursor-only change does not rewrite the bitmap.
- Engine mutations call `set_membership` after re-evaluating each affected subscription's query; debounced events flush on `tick`.
- Format changes are one-way: pools recreate.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Subscriptions

The subscription directory is a BTREE-DESIGN§1 B+ tree region of
`BtreeKind::Subscriptions` with one leaf entry per subscription. The
2-field packed key `(SubscriptionId, snapshot)` (BTREE-DESIGN§1.6) lets the
codec pack the typical "one snapshot dominates" case to near-zero
bits. The value tail is the CBOR-serialised `SubscriptionRecord`,
persisted via `VALUE_SIZE_KIND_VARINT` (BTREE-DESIGN§1.6) — query ASTs and
retention configs are heterogeneous so per-entry value lengths vary.

```rust
// Sorted-run key — 2 fields via SortedRunKeyFormat.
struct SubscriptionsKey {
    id:       u64,                            // SubscriptionId
    snapshot: u32,                            // = 0 until R6
}

// Sorted-run value tail — variable-length CBOR.
#[derive(Serialize, Deserialize)]
struct SubscriptionRecord {                   // CBOR; ~100–400 B typical
    name:          String,                    //   human label
    query:         Query,                     //   subscription's predicate AST
    interest:      ChangeInterest,            //   add/remove/update mask
    cursor:        u64,                       //   last delivered WAL LSN
    state:         SubscriptionState,         //   Active / Paused / Cancelled
    retention:     Retention,                 //   Unlimited / AtMostOnce / Bounded { max_events }
    debounce_ms:   u32,                       //   coalesce window
    cached_result: Vec<u8>,                   //   roaring bitmap byte image (inline)
}
```

**`next_id` is not persisted.** Boot recovers it as
`max(loaded_subscription_ids)` (or `0` on an empty pool). The
allocator is pre-bump (`next_id += 1; id = next_id`), so `next_id`
ends up equal to the last assigned id rather than the next unused
one. This mirrors ENGINE-DESIGN§2's "scalars are derived, not stored" rule for
`next_oid_local` and `next_tag_id`.

**`cached_result` is inlined for R1c.** The eventual spec-target is to
externalize the roaring bitmap to a INDEX-DESIGN§3.2 `TagBitmap` chain referenced
by `BlockRef` — so a cursor-only mutation doesn't rewrite the bitmap
bytes. R1c keeps the bitmap inline in the CBOR record because (a)
per-subscription bitmaps are typically small (a handful of oids), and
(b) externalization needs Tier 3 D3 sub-bucket allocation to avoid
competing with the fixed-offset INDEX-DESIGN§3.2 tag-bitmap area cap.

> TODO(SUBSCRIPTION1): replace `cached_result: Vec<u8>` with a
> `cached_result_root: BlockRef` pointing at a INDEX-DESIGN§3.2 `TagBitmapPage`
> chain. The wire format break is one-way; pre-D3 pools must
> recreate.

**Per-sub COW (Tier 3 B1).** Today's flush rewrites the whole
directory region via `write_full_packed_force_varint`. Once B1 wires
`append_sorted_run` into the production flush path, cursor-bumping
one subscription appends a single 1-entry sorted run instead of
rewriting the region — the per-record key + VARINT value layout is
exactly what makes that work.

