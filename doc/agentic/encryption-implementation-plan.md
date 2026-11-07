# Encryption — implementation plan

Design: [encryption-design.md](encryption-design.md) · Decisions: [encryption-decisions.md](encryption-decisions.md) · Order: [operations-plan.md](operations-plan.md)

- [ ] **ENCRYPTION1 — Cipher modes in the transform.** `Encryptor` implements the SPECIFICATION-DESIGN§9.3 modes instead of returning `TransformError::EncryptionDisabled`. Prerequisite: ENCRYPTION-D1.
- [ ] **ENCRYPTION2 — Key hierarchy.** `MasterKEK → DiskKey`, real `TransformKey` material, `encryption_keyid` in the superblock. Prerequisites: ENCRYPTION-D3, ENCRYPTION-D4.
- [ ] **ENCRYPTION3 — Encrypted journal sets.** `JSET_FLAG_ENCRYPTED` on write and replay, one AES-256-GCM unit per jset under the pool's journal key with the nonce and AAD of ENCRYPTION-DESIGN§1 and JOURNAL-DESIGN§1.2. Prerequisites: ENCRYPTION1, ENCRYPTION2, [JOURNAL9](journal-implementation-plan.md).
- [ ] **ENCRYPTION4 — Encrypted index and metadata blocks.** XTS decrypt and encrypt for blocks with `BLOCK_FLAG_ENCRYPTED`. Prerequisites: ENCRYPTION1, ENCRYPTION2.
- [ ] **ENCRYPTION5 — Encrypted data extents.** HCTR2 on object extents, chunks and packed blocks with the owner-derived tweaks of ENCRYPTION-DESIGN§1. Prerequisites: ENCRYPTION1, [BLOB1](blob-implementation-plan.md). Acceptance: two objects sharing a chunk read it back from one stored copy; an extent moved by copy GC decrypts without re-encryption; identical content in two unchunked objects produces different ciphertext.