# Encryption — design

Scope: `crates/transform` (`Encryptor`), encrypted journal sets in `crates/wal`, encrypted blocks in `crates/storage`, key material in the superblock and pool state.
Governing spec: SPECIFICATION-DESIGN§9.3 (ciphers per bucket kind), SPECIFICATION-DESIGN§9.4 (key hierarchy), SPECIFICATION-DESIGN§9.5 (trust tiers), ENCRYPTION-DESIGN§1.

Plan: [encryption-implementation-plan.md](encryption-implementation-plan.md) · Open decisions: [encryption-decisions.md](encryption-decisions.md) · Completed: [encryption-completed.md](encryption-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Each kind of stored data — node blocks, data extents, journal sets — is encrypted with the cipher SPECIFICATION-DESIGN§9.3 assigns it, under keys derived from one master key, with no change to the plaintext formats.

## Selected design

- **Ciphers:** node-bucket blocks XTS-AES-256 (tweak = block address and bucket generation); data extents HCTR2 (tweak from the owner: `object_id` for private pieces, the piece hash for shared pieces, ENCRYPTION-DESIGN§1); journal sets AES-256-GCM (AAD the `JsetHeader` with its checksum zeroed, nonce `seq || 0`, 16 B tag after the ciphertext, ENCRYPTION-DESIGN§1); sync traffic ChaCha20-Poly1305; Adiantum as the fallback without AES instructions.
- **Key hierarchy:** `MasterKEK → DiskKey` (SPECIFICATION-DESIGN§9.4), with the journal key derived using the pool's `fs_uuid`; the superblock records `encryption_keyid`.
- Sector padding for XTS/HCTR2 is already in the pipeline; journal entries compress before they encrypt.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Encryption Integration

The on-disk layout is encryption-aware but encryption-agnostic:

- **Node buckets** (tree nodes and positional regions, ALLOCATION-DESIGN§1.2): blocks encrypted
  with **XTS-AES-256**, tweak = `(disk_id, bucket_no, generation, sector)` — the block's address
  and the bucket's generation (ALLOCATION-DESIGN§1.3), so a reused bucket never encrypts new
  content under an old tweak, which would expose equal 16-byte blocks across the bucket's
  lives. The tweak follows the location: moving a node (copy GC, RECONCILE-DESIGN§1.5) decrypts
  and re-encrypts it. Fixed layout is preserved (no expansion). Which node blocks are encrypted,
  and whether whole or by run or entry, is [ENCRYPTION-D6](encryption-decisions.md).
- **Data and packed buckets**: encrypted with **HCTR2-AES-128**, length-preserving. The tweak
  depends on who owns the bytes, never on where they are, so moves (RECONCILE-DESIGN§1.5) copy
  ciphertext without re-encrypting:
  - a private piece (METADATA-DESIGN§1.3): `(object_id || block_no_within_object)`, so equal
    content in different objects encrypts differently;
  - a shared piece (INDEX-DESIGN§4.3): `(piece_hash[0..8] || block_no_within_piece)`.
    Deduplication happens at the chunk index before encryption, so each distinct shared piece is
    stored and encrypted once and is shared by every object that references it. The price is
    that equal shared pieces have equal ciphertext: anyone who can read the stored bytes learns
    which objects share content there, though not the content itself. Pieces are private unless
    an object's policy asks for sharing (SPECIFICATION-DESIGN§9.6);
  - a packed small-object block: `(block_id || block_no)`, with the block identifier BLOB-D4
    defines, stable across moves.
- **Journal**: each journal set's entries are **AES-256-GCM** authenticated as one unit when
  `JSET_FLAG_ENCRYPTED` is set (JOURNAL-DESIGN§1.2).
  - **Key**: the pool's journal key from the key hierarchy — one key for the whole journal, so
    mirrored copies of a set on different disks are byte-identical. It is derived with the
    pool's `fs_uuid` as input (ENGINE-DESIGN§1.1): jset sequence numbers restart in every pool,
    so a pool recreated under the same passphrase must not reuse the previous pool's key.
  - **Nonce**: 96 bits = 64-bit jset `seq` || 32-bit zero. Nonces are unique by construction —
    sequence numbers are monotonically allocated and never repeat under one key. AES-GCM is
    *not* a nonce-misuse-resistant scheme; uniqueness must be enforced and reuse is catastrophic.
  - **Sequence wrap is forbidden.** The 64-bit space cannot wrap within any realistic
    deployment lifetime (at 10⁶ sets/s, ~580 000 years). The implementation **panics on `seq`
    overflow** rather than wrapping, since reusing a sequence number under the same key would
    reuse a GCM nonce and break confidentiality and integrity.
  - **AAD**: the `JsetHeader` with its `checksum` field zeroed, authenticating sequence, trim
    point, timestamp base, lengths and flags; re-binding a valid ciphertext to another position
    in the journal is detected as a tag mismatch.
  - **Layout**: the 16-byte tag follows the ciphertext; the set's checksum covers ciphertext and
    tag, so readers without the key still validate I/O integrity.
- **Sync bundles**: **ChaCha20-Poly1305** with random nonces.

Keys never leave the in-memory key hierarchy (`MasterKEK → DiskKey`). The superblock stores only
key *identifiers*.

**Without the keys** a node can read the superblocks and the journal-set headers (authenticated,
not encrypted) and nothing else of an encrypted pool: the root list travels in encrypted journal
sets (ENGINE-DESIGN§1.2), so no tree is reachable. What remains readable of a pool that encrypts
only some objects depends on ENCRYPTION-D6.

**Integrity.** XTS and HCTR2 do not authenticate. Random modification of a block is caught by
its checksum and of object content by its BLAKE3 hash; writing back an older valid version of a
block or bucket is [ENCRYPTION-D7](encryption-decisions.md).

---

