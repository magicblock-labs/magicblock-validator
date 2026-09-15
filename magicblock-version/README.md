# `magicblock-version`

Build and compatibility metadata for validator version reporting.

`Version::default` combines the Cargo package version, optional `CI_COMMIT`
prefix, feature-set identifier, MagicBlock client ID, Solana RPC API version,
and Git-derived version string. An absent or unparseable commit prefix is
represented as zero.

`Display` emits the package's semantic version; `Debug` adds source, feature,
and client information. The `semver!` and `version!` macros expose those two
presentations.

The serialized fields and numeric client identifiers are compatibility data.
A version response describes a build; it does not establish deployment health
or compatibility with a particular upstream replication peer.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)
