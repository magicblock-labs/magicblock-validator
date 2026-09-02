# MagicBlock Configuration

Typed configuration for the MagicBlock leader and verifier binaries.

## Leader

`LeaderParams::try_new` merges configuration in this precedence order:

1. command-line arguments;
2. `MBV_` environment variables;
3. the TOML file passed with `--config`;
4. defaults.

Environment nesting uses `__`, for example
`MBV_ENGINE__LEDGER__SIZE_LIMIT`.

```rust
use magicblock_config::LeaderParams;

let config = LeaderParams::try_new(std::env::args_os())?;
```

[`config.example.toml`](../config.example.toml) documents the complete leader
configuration. `LeaderParams::load` loads the same file and environment layers
without parsing process arguments; operator tools use it to share the leader's
RPC endpoint and signing authority.

The optional `[admin]` section controls periodic administrative work:

```toml
[admin]
claim-fees-frequency = 300
```

Magic Domain Program registration is intentionally not a lifecycle setting.
Use the `mbv domain` commands to register, synchronize, or unregister a leader.

## Verifier

`VerifierParams::try_new` loads the required positional TOML path and overlays
`MBV_VERIFIER_` environment variables. The verifier accepts only follower
engine settings and derives the engine's remote authority from the configured
replication upstream.

See
[`config.verifier.example.toml`](../config.verifier.example.toml) for the
minimal follower configuration.

## Shared engine configuration

`EngineConfig<R>` owns identity, AccountsDB, ledger, block production, and
role-specific replication configuration:

- `EngineConfig<LeaderReplication>` is embedded by `LeaderParams`;
- `EngineConfig<FollowerReplication>` is embedded by `VerifierParams`.

Both process roles use the same `magicblock-runtime` image builder, so builtins,
loadable programs, and genesis accounts stay identical.

## Validation

```bash
cargo test -p magicblock-config
```
