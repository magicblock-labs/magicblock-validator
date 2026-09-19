# magicblock-version

Provides build metadata for validator version reporting, including package,
source, and compatibility identifiers.

Use the shared `Version` type and formatting helpers for consistent reporting
across services. The short display shows the package version; debug output adds
build details useful for diagnostics.

Version metadata identifies a build. It does not establish that a deployment is
healthy or compatible with a particular replication peer.

[Back to workspace](../README.md)
