# `magicblock-config`

Typed configuration for the leader, verifier, and operator tooling. The role
determines the accepted settings; replication is not a leader `lifecycle` mode.

## Leader

`LeaderParams::try_new` merges settings from highest to lowest precedence:

1. command-line arguments;
2. `MBV_` environment variables;
3. the TOML file selected with `--config`;
4. defaults.

Nested environment keys use `__`, for example
`MBV_METRICS__ADDRESS=127.0.0.1:9090`. Missing remote endpoint types can be
filled from defaults or derived URLs; check the effective configuration before
assuming which base-chain services will be contacted.

`LeaderParams::load` applies file/environment layers without parsing the
process CLI. The operator commands use it to obtain the base-chain RPC endpoint
and local signing identity.

The [leader example][leader-config] documents service, account synchronization,
storage, replication, plugin, and task settings. `[admin]` enables periodic
fee claims. Domain registration remains an explicit operator action.

## Verifier

`VerifierParams::try_new` requires a positional TOML path and overlays
`MBV_VERIFIER_` variables with the same nested-key convention. Its configuration
contains metrics, follower Engine settings, and startup programs, not the
leader's application-service graph.

Remote authority is derived from `replication.upstream-authority`; explicitly
supplying `engine.authority.remote` is rejected. The upstream identity is
different from the verifier's local signing identity.

Use the [verifier example][verifier-config] as the starting point, replacing
sample identities and addresses.

## Shared runtime inputs

`EngineConfig<R>` carries authority, account storage, ledger, block production,
and role-specific replication settings. Both roles pass their startup programs
through the [shared runtime builder][runtime]. The actual ELF artifacts must
match across hosts; matching configuration structure alone is insufficient.

Configuration names, merge precedence, and validation errors are operator-facing
interfaces. Keep changes synchronized with the example files and binary usage.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[leader-config]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/config.example.toml
[verifier-config]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/config.verifier.example.toml
[runtime]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-runtime/README.md
