# `mbv`

`bins/mbv` is the extensible manual operator CLI.

The `domain register`, `domain sync`, and `domain unregister` commands are the
only owners of Magic Domain Program lifecycle interactions. They load the
leader TOML plus `MBV_` overlays to reuse its base-layer RPC endpoint and local
authority. Country and public address are explicit command flags.

Keep operator transactions explicit. Do not turn these commands into leader
startup/shutdown hooks or background services.

Focused validation:

```bash
cargo check -p mbv --locked
```
