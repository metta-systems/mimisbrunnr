# Encryption — open decisions

Design: [encryption-design.md](encryption-design.md) · Plan: [encryption-implementation-plan.md](encryption-implementation-plan.md)

## ENCRYPTION-D1 — Cipher dependencies

Adding `aes`, `xts-mode`, `hctr2`, `aes-gcm` and `chacha20poly1305` is a workspace dependency change. Approve the crate set (and an Adiantum implementation if the fallback is in scope now). Blocks [ENCRYPTION1](encryption-implementation-plan.md).

## ENCRYPTION-D3 — Where keys are stored

SPECIFICATION-DESIGN§9.4 shows the key hierarchy but no on-disk home for wrapped keys. Decide the storage location (superblock fields, a key block in the pool state, an external keystore) and the rewrap procedure.

## ENCRYPTION-D4 — Trust tier semantics

SPECIFICATION-DESIGN§9.5 "semi-trusted" loses the metadata key on reboot, yet keys are not session keys and could be reloaded. Define each trust tier by which keys are held where and when, and what an attacker at each tier obtains.

## ENCRYPTION-D5 — Per-tag keys

SPECIFICATION-DESIGN§9.4 mentions per-tag keys without saying what they encrypt or how they combine with zone keys. Decide whether per-tag keys exist and their role.

## Future work

- None recorded.
