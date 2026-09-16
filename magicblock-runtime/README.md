# magicblock-runtime

Builds the shared startup image used by leaders and verifiers: native programs,
configured program executables, and initial accounts.

## Integration

Both processes use `keeper_builder` to prepare the image before opening Engine.
This crate does not start application services or manage process shutdown.

Provide matching program IDs and executable files on the leader and verifier.
Sharing a builder does not make separately deployed artifacts identical.
Configured files must be available at startup.

See [configuration](../magicblock-config/README.md) for inputs and the
[leader](../bins/magicblock-validator/README.md) and
[verifier](../bins/magicblock-verifier/README.md) guides for running each role.

[Back to workspace](../README.md)
