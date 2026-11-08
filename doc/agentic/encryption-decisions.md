# Encryption — open decisions

Design: [encryption-design.md](encryption-design.md) · Plan: [encryption-implementation-plan.md](encryption-implementation-plan.md)

## ENCRYPTION-D1 — Cipher dependencies

Adding `aes`, `xts-mode`, `hctr2`, `aes-gcm` and `chacha20poly1305` is a workspace dependency change. Approve the crate set (and an Adiantum implementation if the fallback is in scope now). Blocks [ENCRYPTION1](encryption-implementation-plan.md).

## ENCRYPTION-D3 — Where keys are stored

SPECIFICATION-DESIGN§9.4 shows the key hierarchy but no on-disk home for wrapped keys. Decide the storage location (superblock fields, a key block in the pool state, an external keystore) and the rewrap procedure.

## ENCRYPTION-D4 — Trust tier semantics

SPECIFICATION-DESIGN§9.5 "semi-trusted" loses the metadata key on reboot, yet keys are not session keys and could be reloaded. Define each trust tier by which keys are held where and when, and what an attacker at each tier obtains.

## ENCRYPTION-D5 — Per-tag keys

SPECIFICATION-DESIGN§9.4 mentions per-tag keys without saying what they encrypt or how they combine with the pool's node, data and journal keys. Decide whether per-tag keys exist and their role.

## ENCRYPTION-D6 — Transparent, per-object encryption

The goal is transparent partial encryption: a pool holds encrypted and unencrypted objects side by side, and an object can switch between them, without the pool as a whole being "encrypted" or not. Investigate and decide:

- **What an object's encryption covers.** Its data only, or also its object record, forward entry and the values of its attribute terms. Plaintext metadata for an encrypted object leaks its tags, names and sizes; encrypted metadata means node blocks mix encrypted and unencrypted content.
- **Granularity in node blocks.** Whole blocks encrypted per tree or per pool, encryption per run, or per entry — and how a reader knows (a pool feature flag, `RUN_FLAG_ENCRYPTED` (BTREE-DESIGN§1.2), a per-entry bit). Positional record pages hold 32 records of possibly different objects.
- **The record's encryption field.** `ObjectRecord.encryption` names a cipher, but data always uses HCTR2; what matters is the key scope (none, pool key, tag key — ENCRYPTION-D5) and a key id, perhaps from the record's reserved bytes.
- **Switching.** Reconcile rewrites an object's extents under the new policy (RECONCILE-DESIGN§1.1); its old plaintext extents remain readable until their buckets are reused, as for inline objects (METADATA-DESIGN§1.2 "Residual copies"). Decide whether that is documented or scrubbed.
- **Terms shared across objects.** An attribute value used by an encrypted and an unencrypted object is one term; its value in the term table is either encrypted or not.
- **Journal.** Whether journal sets are always encrypted when any object is, since they carry every object's mutations.

Shapes [ENCRYPTION1](encryption-implementation-plan.md), [ENCRYPTION4](encryption-implementation-plan.md), [ENCRYPTION5](encryption-implementation-plan.md) and ENCRYPTION-D5.

## ENCRYPTION-D7 — Rollback of blocks and buckets

An attacker with the disk can write back an older valid version of a node block or a whole bucket. XTS and HCTR2 do not authenticate, checksums are unkeyed, and generations do not help because the bucket table can be rolled back with the rest. Options: (a) declare rollback out of the threat model and say so in SPECIFICATION-DESIGN§9; (b) authenticate the root list with a keyed MAC and a monotonic counter kept outside the pool (a TPM or the Mettā key store), and node pointers with keyed hashes of their targets, so any rollback below the root is detected.

## Future work

- None recorded.
