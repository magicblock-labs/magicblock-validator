# `magicblock-config`

This crate owns typed process configuration.

## Public roots

- `LeaderParams` combines `EngineConfig<LeaderReplication>` with leader-only
  service settings. `try_new` merges CLI, `MBV_` environment, TOML, and
  defaults. `load` applies TOML and environment without a CLI overlay for
  operator tools.
- `VerifierParams` contains a required metrics configuration,
  `EngineConfig<FollowerReplication>`, and loadable programs. `try_new`
  requires a positional TOML file and overlays `MBV_VERIFIER_`.
- `EngineConfig<R>` keeps the role generic at the replication field.

Leader configuration rejects a remote authority. Verifier configuration
rejects a user-supplied remote authority and derives it from the replication
upstream, keeping authentication identity authoritative in one place.

The optional `AdminConfig` contains periodic administrative settings such as
fee claiming. Domain country/address fields do not belong in configuration;
they are manual `mbv domain` inputs.

Configuration is a compatibility boundary. Preserve serde names, strict
unknown-field rejection, secret-redacted debug output, and validation unless
the operator contract is intentionally changed.

Focused validation:

```bash
cargo test -p magicblock-config
```
