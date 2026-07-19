2.1 NodeId = how to guarantee unique node assignment in the cluster?

4.2 For ontology: try kdl instead of toml? make claude attempt a good syntax for that.

```toml
[[tags]]
name = "artist"
semantics = "attribute"       # or "attr"
value_type = "text"           # default; also "int", "float", "timestamp", "blob"

[[tags]]
name = "playlist"
semantics = "ordered-collection"   # or "ordered"
```

become

```kdl
module "systems.metta.music"
version "2.1.0"
name "Music Ontology"

tags {
  "artist" text attribute
  "playlist" ordered-collection
  "genre" grouping
}

implications {
  from "rock" to "genre"
  from "flac" to "audio"
}
```

6.2 object record growth is almost unbounded, might it just become a huge blob of Cleared records at the beginning? need some compaction strategy (e.g. RLE, but without breaking the simple radix indexing).

Pull in overflow record layout from IMPLEMENTATION.md, now its only referenced.

6.4 encryption is very cryptically desribed, "nonce = 64-bit LSN || 32-bit zero (WAL)" is unclear - why?

7.4 explain "HLC ordering" better, without acronyms, add link to clock section (hlc is defined in 10.4)

7.5 bulk deletion does not mention "tag count" field of each object record - mass deleting a tag will need to update this count EVERYWHERE? that doesn't scale

8.2 shouldn't Disk 1 (SSD) also have a replica of Index and Metadata zones? they need to be replicated in the cluster too.

8.3 says chunking policy is an admin override, but then places it at the lower priority than ontology setting.
more chunking explanations in 9.6 - maybe link the sections properly.

9 "Transform pipeline" is a weird name, find a better one.

9.1 encryption for AES-GCM is unclear - "authenticated; nonce = 64-bit LSN || 32-bit zero (WAL)"

9.1 TransformResult seems inappropriate here, is that on-disk format or what?

9.2 compression granularity is hand-wavy "file to block" sized but no explanation how it works

9.3 mentions use of AES-256-GCM then says AES-GCM is NOT used. Clarify.

9.4 shows the key hierarchy, but double check we have the place to store those keys in the superblock or some key structure? I don't remember seeing them there.

9.4 per-tag keys must be explained betterer (and where/how they are used)

9.5 semi-trusted is weird - if it loses metadata key on reboot, it can just load the old key again, those are not session keys? clarify all trust tiers.

10.2 should we use iroh as a sync transport protocol?

10.3 hydration models are unclear, why is there Partial if we already have Hydrating with progress? clarify.

10.7 conflict resolution details are not specified.

10.9 look at ipfs-frontend implementation and try to bring it somewhere outside of metta

11.2-inspired - all mp3 files are "audio" but not all mp3 files are "music", how to model this?
> see chat with claude on this, there is a good enough solution

❓ in addition, extend this to "text" and "email" categories perhaps, to test it out (also, picture/movies albums/collections)

11.9 provide a more complex example, when an incremental rebuild also has 3P dependencies, each with their own "projects" etc?

12.3 expand on Value::Scoped and scoped tags here, that is probably important machinery.

13.1 clarify on derivations, clarify on stale and dirty lifecycle tags

13.2 builds a hierarchy on top of non-hierarchical system - this seems wrong; i would go with intersecting categories (low-height trees)

15 seems to be from IMPLEMENTATION.md

16 architecture should probably be moved to the very beginning of the document, with following sections expanding on the layers

16 "Extended oplog retention for dormant subscriptions" must be better explained (in subscriptions section)
