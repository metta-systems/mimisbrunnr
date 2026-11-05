# Encryption — design

Scope: `crates/transform` (`Encryptor`), encrypted WAL entries in `crates/wal`, encrypted blocks in `crates/storage`, key material in the superblock and pool state.
Governing spec: SPECIFICATION-DESIGN§9.3 (ciphers per zone), SPECIFICATION-DESIGN§9.4 (key hierarchy), SPECIFICATION-DESIGN§9.5 (trust tiers), ENCRYPTION-DESIGN§1.

Plan: [encryption-implementation-plan.md](encryption-implementation-plan.md) · Open decisions: [encryption-decisions.md](encryption-decisions.md) · Completed: [encryption-completed.md](encryption-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every zone is encrypted with the cipher SPECIFICATION-DESIGN§9.3 assigns it, under keys derived from one master key, with no change to the plaintext formats.

## Selected design

- **Ciphers:** index and metadata blocks XTS-AES-256 (tweak `block_no`); blob extents HCTR2 (tweak `object_id || block_no_within_extent`); WAL AES-256-GCM (AAD `header[0..36]`, nonce `LSN || 0`, 16 B tag before the trailing CRC); sync traffic ChaCha20-Poly1305; Adiantum as the fallback without AES instructions.
- **Key hierarchy:** `MasterKEK → DiskKey` (SPECIFICATION-DESIGN§9.4); the superblock records `encryption_keyid`.
- The per-pool SipHash secret for `value_hash` derives from `DiskKey`.
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
- **WAL**: each entry is **AES-256-GCM** authenticated when `WAL_ENTRY_FLAG_ENCRYPTED` is set.
  - **Nonce**: 96 bits = 64-bit LSN || 32-bit zero. Nonces are unique by construction — LSNs
    are monotonically allocated and never repeat under one key. AES-GCM is *not* a
    nonce-misuse-resistant scheme; uniqueness must be enforced and reuse is catastrophic.
  - **LSN wrap is forbidden.** The 64-bit LSN space (~1.8 × 10¹⁹ values) cannot wrap within
    any realistic deployment lifetime — at 10⁶ entries/s sustained, wrap takes ~580 000
    years. The implementation **panics on `next_lsn` overflow** rather than wrapping; this
    is treated as an unrecoverable invariant violation, since reusing an LSN under the same
    `DiskKey` would reuse a GCM nonce (ENCRYPTION-DESIGN§1) and break confidentiality + integrity.
    Operationally there is no "near-the-boundary" code path to test or rotate around.
  - **AAD**: the first 36 bytes of `WalEntryHeader` — every field except `payload_crc` —
    authenticating magic, op_kind, format_version, flags, lsn, timestamp, and payload_length.
    Header-swap attacks (re-binding a valid ciphertext to a different LSN or op_kind) are
    therefore detected as tag mismatches.
  - **Layout**: the 16-byte GCM tag is appended to the ciphertext, before the trailing 4-byte
    framing CRC; see JOURNAL-DESIGN§1.2 for the full byte layout and read/write ordering. `payload_crc`
    remains a 4-byte CRC32C — over ciphertext in encrypted mode, over plaintext otherwise —
    so readers without the key still validate framing/I/O integrity.
- **Sync bundles**: **ChaCha20-Poly1305** with random nonces.

Keys never leave the in-memory key hierarchy (`MasterKEK → DiskKey`). The superblock stores only
key *identifiers*. A node booting without the master key still mounts the disk read-only at the
unencrypted-layout level (block headers, but not payloads).

---

