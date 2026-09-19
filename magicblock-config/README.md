# magicblock-config

Configure a leader, verifier, or operator command using TOML files and overrides.
This crate loads and validates those settings.

Start with the example for the process you want to run:

- [Leader configuration](../config.example.toml): application services, remote
  providers, storage, and replication.
- [Verifier configuration](../config.verifier.example.toml): upstream replication,
  local storage, identity, and startup programs.

Replace sample identities, credentials, addresses, and program paths before use.

## Overrides

Leader settings are applied in this order, highest priority first:

1. Explicit command-line setting overrides.
2. `MBV_` environment variables.
3. TOML settings.
4. Defaults.

`--config` selects the TOML file; it does not give that file priority over
environment variables.

The verifier takes a positional TOML path and uses `MBV_VERIFIER_` environment
overrides. Both prefixes use `__` between nested keys, for example
`MBV_METRICS__ADDRESS=127.0.0.1:9090`.

For operator commands, the exact file supplied with `--config` must exist;
`MBV_` overrides and defaults then apply. The leader can run without a config
file, but connects to **devnet** by default. It still needs reachable providers,
the necessary base-chain programs and accounts, and a funded identity.
For a self-contained development chain, use the
[packaged stack](../.github/packages/npm-package/README.md).

Fee claiming is an [explicit operator command](../bins/magicblock/README.md#fee-claims),
not a validator service. Remove legacy `[admin]` configuration and `MBV_ADMIN__*`
overrides and schedule claims externally if needed.

## Running both roles

Give each process its own identity, storage, and listener addresses. The
verifier's upstream authority identifies the leader it follows, not its own
signing identity. Set it through `engine.replication.upstream-authority`, not
`engine.authority.remote`: the verifier fills in the latter automatically and
rejects an explicit value. Leaders also reject `engine.authority.remote`.

Supply matching program IDs and executable files to both. A verifier with a
distinct signing identity can follow but cannot relay; see the
[verifier relay restriction](../bins/magicblock-verifier/README.md#monitoring-and-recovery).

See the [leader](../bins/magicblock-validator/README.md) and
[verifier](../bins/magicblock-verifier/README.md) guides for launch commands.

[Back to workspace](../README.md)
