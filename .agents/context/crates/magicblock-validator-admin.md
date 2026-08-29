# `magicblock-validator-admin`

This crate owns leader administrative helpers that submit authorized
base-layer transactions.

`ClaimFeesTask` and `claim_fees` are consumed by
`bins/mbv-leader/src/leader.rs`. The leader performs a best-effort one-shot
claim during ephemeral startup and can run periodic claims while active. Both
are enabled only when `[admin] claim-fees-frequency` is present and non-zero.
The task is stopped before engine shutdown.

Magic Domain Program registration is not owned here and is not part of the
leader lifecycle. Manual registration, synchronization, and unregistration live
in `bins/mbv`.

Preserve explicit signer requirements, bounded RPC behavior, and idempotent
fee-vault handling. Do not move general protocol execution or lifecycle
orchestration into this crate.

Relevant validation:

```bash
cargo check -p magicblock-validator-admin -p mbv-leader --locked
```

See `.agents/context/crates/mbv-leader.md` for lifecycle ownership and
`.agents/context/crates/magicblock-config.md` for `[admin]` semantics.
