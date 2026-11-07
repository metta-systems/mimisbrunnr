# Encryption — design

Scope: `crates/transform` (`Encryptor`), encrypted WAL entries in `crates/wal`, encrypted blocks in `crates/storage`, key material in the superblock and pool state.
Governing spec: SPECIFICATION-DESIGN§9.3 (ciphers per zone), SPECIFICATION-DESIGN§9.4 (key hierarchy), SPECIFICATION-DESIGN§9.5 (trust tiers), ENCRYPTION-DESIGN§1.

Plan: [encryption-implementation-plan.md](encryption-implementation-plan.md) · Open decisions: [encryption-decisions.md](encryption-decisions.md) · Completed: [encryption-completed.md](encryption-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every zone is encrypted with the cipher SPECIFICATION-DESIGN§9.3 assigns it, under keys derived from one master key, with no change to the plaintext formats.

## Selected design

- **Ciphers:** index and metadata blocks XTS-AES-256 (tweak `block_no`); blob extents HCTR2 (tweak `object_id || block_no_within_extent`); WAL AES-256-GCM (AAD `header[0..36]`, nonce `LSN || 0`, 16 B tag before the trailing CRC); sync traffic ChaCha20-Poly1305; Adiantum as the fallback without AES instructions.
- **Key hierarchy:** `MasterKEK → DiskKey` (SPECIFICATION-DESIGN§9.4); the superblock records `encryption_keyid`.
- Sector padding for XTS/HCTR2 is already in the pipeline; WAL entries compress before they encrypt.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Encryption Integration

The on-disk layout is encryption-aware but encryption-agnostic:

- **Index zone, metadata zone**: blocks encrypted with **XTS-AES-256**, tweak = `block_no`. Fixed
  layout is preserved (no expansion). `BlockHeader` is encrypted along with payload — verifiers
  decrypt then check magic.
- **Blob zone**: encrypted with **HCTR2-AES-128**, tweak = `(object_id || block_no_within_extent)`.
  Length-preserving.
- **Journal**: each journal set's entries are **AES-256-GCM** authenticated as one unit when
  `JSET_FLAG_ENCRYPTED` is set (JOURNAL-DESIGN§1.2).
  - **Key**: the pool's journal key from the key hierarchy — one key for the whole journal, so
    mirrored copies of a set on different disks are byte-identical.
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
key *identifiers*. A node booting without the master key still mounts the disk read-only at the
unencrypted-layout level (block headers, but not payloads).

---

