# magicblock-config

Loads and validates configuration for the leader, verifier, and operator tools.

Start with the example for the process you want to run:

- [Leader configuration](../config.example.toml): application services, remote
  providers, storage, and replication.
- [Verifier configuration](../config.verifier.example.toml): upstream replication,
  local storage, identity, and startup programs.

Replace sample identities, credentials, addresses, and program paths before use.

## Overrides

Leader settings are applied in this order, highest priority first:

1. Command-line options, including the file selected with `--config`.
2. `MBV_` environment variables.
3. TOML settings.
4. Defaults.

The verifier takes a positional TOML path and uses `MBV_VERIFIER_` environment
overrides. Both prefixes use `__` between nested keys, for example
`MBV_METRICS__ADDRESS=127.0.0.1:9090`.

## Running both roles

Give each process its own identity, storage, and listener addresses. The
verifier's upstream authority identifies the leader it follows, not its own
signing identity. Supply matching program IDs and executable files to both.

See the [leader](../bins/magicblock-validator/README.md) and
[verifier](../bins/magicblock-verifier/README.md) guides for launch commands.

[Back to workspace](../README.md)
