# Encryption — implementation plan

Design: [encryption-design.md](encryption-design.md) · Decisions: [encryption-decisions.md](encryption-decisions.md) · Order: [operations-plan.md](operations-plan.md)

- [ ] **ENCRYPTION1 — Cipher modes in the transform.** `Encryptor` implements the SPECIFICATION-DESIGN§9.3 modes instead of returning `TransformError::EncryptionDisabled`. Prerequisite: ENCRYPTION-D1.
- [ ] **ENCRYPTION2 — Key hierarchy.** `MasterKEK → DiskKey`, real `TransformKey` material, `encryption_keyid` in the superblock. Prerequisites: ENCRYPTION-D3, ENCRYPTION-D4.
- [ ] **ENCRYPTION3 — Encrypted WAL entries.** `WAL_ENTRY_FLAG_ENCRYPTED` is accepted on append and replay with the AAD, nonce and tag framing of ENCRYPTION-DESIGN§1. Prerequisites: ENCRYPTION1, ENCRYPTION2.
- [ ] **ENCRYPTION4 — Encrypted index and metadata blocks.** XTS decrypt and encrypt for blocks with `BLOCK_FLAG_ENCRYPTED`. Prerequisites: ENCRYPTION1, ENCRYPTION2.
- [ ] **ENCRYPTION5 — Encrypted blob extents.** HCTR2 on blob extents. Prerequisites: ENCRYPTION-D2, [BLOB1](blob-implementation-plan.md).