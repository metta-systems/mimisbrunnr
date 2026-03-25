# High-level concepts

Superblock
Object Record and Object Index
Storage Layering - sectors, extents, pools, indexes, WAL

seaweedfs.topology.VolumeLayout - descriptors of what is where in the volume set
- VolumeServer → Store → DiskLocation → Volume → Needle

Erasure Coding:

- Convert volumes to shards
- Default: 10 data + 4 parity shards (can withstand 4 failures)
- Reduces storage cost by ~40% vs. 2x replication
- Reed-Solomon encoding
- Rebuild missing shards
