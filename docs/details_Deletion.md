This is one of those problems that looks simple but has nasty edge cases once you add cluster sync. Let me work through it carefully.

## The Tension

Object IDs appear in three places that all need to agree:

```
1. Every roaring bitmap in the tag index    (potentially thousands of bitmaps)
2. The forward index                        (obj → assertions)
3. The metadata zone                        (object record array)
4. The location table                       (obj → disk extent)
5. Other nodes' replicated copies of 1-4
6. In-flight sync ops referencing this ID
7. Chunk index entries (if chunked)
8. Ordered collection sequences             (Vec<ObjectId> in playlists etc.)
```

If you recycle ID 42 and a remote node hasn't yet processed the deletion, it will apply new tags meant for the _new_ object 42 to the _old_ object 42's metadata. Corruption.

## The Simple Answer: Don't Recycle

With 48 bits of local sequence per node, each node can create 281 trillion objects before exhaustion. At 1000 objects per second continuously, that's ~8,900 years. ID exhaustion is not a real problem.

```rust
impl ObjectId {
    // Node gets 48-bit local namespace = 281,474,976,710,656 IDs
    // At 1000 creates/sec = 8,925 years before exhaustion
    fn new(node: u16, local_seq: u48) -> Self {
        Self((node as u64) << 48 | local_seq as u64)
    }
}
```

But "don't recycle IDs" doesn't mean deletion is simple. You still need to clean up all the references.

## Deletion Protocol

### Phase 1: Tombstone

Deletion starts by marking the object as deleted, not by removing anything:

```rust
struct ObjectRecord {
    // ... existing fields ...
    state: ObjectState,
}

enum ObjectState {
    Active,
    Deleted {
        deleted_at: HybridTimestamp,
        // Grace period: don't reclaim until all nodes have seen the deletion
        tombstone_expires: HybridTimestamp,
    },
}
```

The tombstone is a sync op like any other:

```rust
SyncOpKind::DeleteObject {
    id: ObjectId,
    deleted_at: HybridTimestamp,
}
```

At this point, the object is invisible to queries but all its data structures still exist. This is fast — O(1), just a state flip in the object record + WAL entry.

### Phase 2: Index Cleanup (Local, Background)

A background task removes the deleted ID from all indexes:

```rust
fn cleanup_deleted_object(id: ObjectId) {
    // 1. Get all tags for this object from forward index
    let assertions = forward_index.get(id);
    
    // 2. Remove from every tag bitmap
    for assertion in &assertions {
        match assertion {
            Assertion::Tag(tag_id) => {
                match tag_store.get_mut(tag_id) {
                    TagStore::Simple(bitmap) => {
                        bitmap.remove(id as u32);
                    }
                    TagStore::Ordered { members, sequence } => {
                        members.remove(id as u32);
                        sequence.retain(|&oid| oid != id);
                    }
                    TagStore::Ranked { members, ranked } => {
                        members.remove(id as u32);
                        ranked.retain(|(oid, _)| *oid != id);
                    }
                }
            }
            Assertion::Attr { key, value } => {
                // Remove from KV equality index
                kv_index.remove(key, value, id);
                // Remove from range index if applicable
                range_index.remove(key, value, id);
            }
            Assertion::Relation { predicate, target } => {
                // Remove from relation indexes (both directions)
                relation_forward.remove(id, predicate);
                relation_reverse.remove(predicate, target, id);
            }
        }
    }
    
    // 3. Remove forward index entry
    forward_index.remove(id);
    
    // 4. Mark blob extent as freeable (but don't free yet)
    let location = location_table.get(id);
    location_table.mark_reclaimable(id);
}
```

The key insight: we iterate the forward index (which tells us exactly which bitmaps contain this ID) rather than scanning all bitmaps. This is O(tags_on_object), not O(total_tags).

For an object with 20 tags, that's 20 bitmap mutations — maybe 2 μs total. Not worth parallelizing.

### Phase 3: Blob Reclamation

The actual disk extent isn't freed immediately. Two reasons:

**Cluster safety.** Another node might still be reading this blob (hydrating it, or mid-transfer). The tombstone grace period handles this.

**Crash safety.** If you free the extent and crash before the WAL checkpoints, recovery could see a committed object pointing to freed space.

```rust
fn reclaim_blob(id: ObjectId, pool: &mut Pool) {
    let location = location_table.get(id);
    
    match location {
        ObjectContent::Direct { disk_id, offset, length } => {
            pool.free_extent(disk_id, offset, length);
        }
        ObjectContent::Chunked { chunk_list_offset, chunk_count, .. } => {
            // Decrement refcount on each chunk
            let chunks = read_chunk_list(chunk_list_offset, chunk_count);
            for chunk in chunks {
                let refs = chunk_store.decrement_ref(chunk.hash);
                if refs == 0 {
                    // Last reference gone — free chunk storage
                    pool.free_extent(chunk.disk_id, chunk.offset, chunk.length);
                    chunk_store.remove(chunk.hash);
                }
            }
        }
    }
    
    // Clear location table entry
    location_table.clear(id);
    
    // Clear object record (or just leave it — 128 bytes, marked deleted)
    object_table.clear(id);
}
```

### Phase 4: Tombstone Expiry

Tombstones can't live forever or the object table grows without bound. But they must live long enough for all nodes to see the deletion.

```rust
fn can_expire_tombstone(
    id: ObjectId, 
    deleted_at: HybridTimestamp,
    cluster: &ClusterState,
) -> bool {
    // All nodes have acknowledged sync ops past this deletion
    let all_caught_up = cluster.all_watermarks_past(deleted_at);
    
    // And a minimum grace period has elapsed (for safety)
    let grace_elapsed = now() - deleted_at > Duration::days(7);
    
    all_caught_up && grace_elapsed
}
```

After expiry, the tombstone slot in the object record is truly free. But we still **don't reuse the ID**. The slot is zeroed and marked as `Never_Used` or `Expired_Tombstone`. The array stays sparse.

## The Sparse Array Problem

Over time, deletions create holes in the object record array:

```
Object records (array indexed by ID):

 0    1    2    3    4    5    6    7    8    9    ...
[ACT][DEL][ACT][   ][ACT][DEL][DEL][ACT][ACT][   ] ...
               ↑                              ↑
               never created                  never created
```

At 128 bytes per slot, a million deleted objects waste 128 MB. Is this a problem?

Not really. For realistic workloads:

```
10M objects created over system lifetime
1M deletions (10% churn)
Waste: 1M × 128B = 128 MB
Total metadata zone: 10M × 128B = 1.28 GB

Overhead: ~10% — acceptable
```

If churn is extreme (50%+ deletion rate), two mitigations:

### Compaction (Offline)

A maintenance operation that compacts the object table, remapping IDs:

```rust
fn compact_object_table(pool: &mut Pool) -> IdRemapTable {
    let mut remap = HashMap::new();
    let mut write_pos = 0u64;
    
    for read_pos in 0..max_object_id {
        let record = object_table.get(read_pos);
        if record.state == ObjectState::Active {
            if write_pos != read_pos {
                remap.insert(read_pos, write_pos);
            }
            write_pos += 1;
        }
    }
    
    // Now update EVERYTHING that references object IDs:
    // - every bitmap in every tag index
    // - every forward index entry
    // - every ordered collection sequence
    // - every relation index
    // - every chunk refcount entry
    // This is expensive! O(total_data)
    
    remap
}
```

This is an offline operation, like `btrfs balance` or ZFS scrub. You'd run it maybe once a year. And it requires a cluster-wide epoch bump to invalidate all cached IDs on remote nodes.

Honestly, it's probably not worth implementing unless your workload has pathological churn. The 48-bit ID space and 128-byte records make sparsity cheap.

### Generation Counter (Lightweight Alternative)

Instead of compaction, add a generation counter to the object record:

```rust
struct ObjectRecord {
    id: u64,
    generation: u32,   // incremented if this slot is ever reused
    // ... rest of fields
}

// In all references to objects (bitmaps don't need this,
// but sync ops and collection sequences do):
struct ObjectRef {
    id: ObjectId,
    generation: u32,
}
```

If you ever _do_ reuse a slot (in a future version, under extreme pressure), stale references are caught by generation mismatch. This is the same trick that ECS systems (bevy, hecs) and slab allocators use:

```rust
fn resolve_object(reference: ObjectRef) -> Option<&ObjectRecord> {
    let record = object_table.get(reference.id);
    if record.generation == reference.generation && record.state == Active {
        Some(record)
    } else {
        None  // stale reference — object was deleted and possibly reused
    }
}
```

This costs 4 bytes per object record (generation field). Cheap insurance.

## Interaction with Cluster Sync

The tricky scenario:

```
Time 0:  Node A creates object 42, tags it "music"
Time 1:  Sync: Node B learns about object 42
Time 2:  Node A deletes object 42
Time 3:  Node B (hasn't seen deletion yet) tags object 42 "favorite"
Time 4:  Sync: Node B receives deletion of 42
Time 5:  Sync: Node A receives "add tag favorite to 42"
```

What happens at time 5? Node A sees a tag-add for a deleted object. Resolution:

```rust
fn apply_sync_op(op: &SyncOp) -> SyncResult {
    match &op.op {
        SyncOpKind::AddTag { object, tag } => {
            let record = object_table.get(*object);
            
            match record.state {
                ObjectState::Active => {
                    // Normal path
                    add_tag(*object, *tag);
                    SyncResult::Applied
                }
                ObjectState::Deleted { deleted_at, .. } => {
                    if op.timestamp < deleted_at {
                        // Op was generated BEFORE deletion — would have been
                        // valid at the time. Drop silently.
                        SyncResult::DroppedStale
                    } else {
                        // Op generated AFTER deletion — shouldn't happen
                        // with correct HLC ordering. Log warning, drop.
                        warn!("Tag add for deleted object {} from node {}", 
                              object, op.origin_node);
                        SyncResult::DroppedConflict
                    }
                }
            }
        }
        // ... other ops
    }
}
```

The HLC ordering makes this deterministic. Since the deletion happened at time 2 and the tag-add at time 3, the deletion's HLC is earlier. When Node A replays in HLC order, it processes the deletion first, then drops the tag-add as stale.

But there's a subtle case: what if the tag-add's HLC is _earlier_ than the deletion (the add was created at time 1.5, before the deletion at time 2, but arrived after)? Then the add is applied first, then the deletion removes it. Correct behavior — the tag briefly existed, then the object was deleted along with all its tags.

## Bulk Deletion

Deleting a tag (and all its associations) is much faster than deleting objects individually:

```rust
fn delete_tag(tag_id: TagId) {
    // 1. Get the bitmap — this IS the list of affected objects
    let bitmap = tag_index.remove(tag_id);
    
    // 2. Remove this tag from each object's forward index
    for obj_id in bitmap.iter() {
        forward_index.remove_assertion(obj_id, Assertion::Tag(tag_id));
    }
    
    // 3. Remove from ontology
    ontology.remove_tag(tag_id);
    
    // 4. Sync op
    emit_sync_op(SyncOpKind::DeleteTag { id: tag_id });
    
    // Objects themselves are untouched — they just lost one tag
}
```

Deleting a tag doesn't delete its member objects. Deleting a playlist removes the grouping, not the music.

## Summary

```
Deletion lifecycle:

  Delete request
       │
       ▼
  ┌──────────────┐
  │ Tombstone    │  Object marked deleted, invisible to queries
  │ (instant)    │  Sync op emitted to cluster
  └──────┬───────┘
         │ background
         ▼
  ┌──────────────┐
  │ Index cleanup│  Remove from all bitmaps, forward index,
  │ O(tags/obj)  │  collection sequences
  └──────┬───────┘
         │ after grace period (all nodes caught up)
         ▼
  ┌──────────────┐
  │ Blob reclaim │  Free disk extents (or decrement chunk refs)
  │              │  Clear location table entry
  └──────┬───────┘
         │ after tombstone expiry
         ▼
  ┌──────────────┐
  │ Slot cleared │  128-byte record zeroed
  │ ID never     │  ID NOT reused (48-bit space is sufficient)
  │ reused       │  Generation counter protects against future reuse
  └──────────────┘
```

Don't recycle IDs. The math works out — 281 trillion per node is enough. The generation counter is there as insurance for a future where it isn't.
