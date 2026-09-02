# `magicblock-runtime`

`magicblock-runtime` owns the authoritative Keeper runtime image shared by
leaders and verifiers.

`keeper_builder` installs:

- the native System and Magic Program builtins;
- every configured BPF program;
- the Magic context, ephemeral vault, and native mint genesis accounts.

Do not duplicate this construction in either binary. Role-specific lifecycle,
storage configuration, and replication remain with the binaries and the
sibling engine.

Focused validation:

```bash
cargo check -p magicblock-runtime --locked
```
