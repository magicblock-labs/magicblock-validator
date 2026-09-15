# `magicblock-runtime`

Builds the Keeper startup image shared by the validator leader and verifier.
Both processes call `keeper_builder` so their native programs and initial
account construction come from one implementation.

## Image construction

The builder combines role-specific Engine authority/storage settings with:

- native Magic, crank, callback, and ephemeral-system entrypoints;
- configured program ELF files;
- initial accounts derived from those programs and the shared runtime setup;
- default rent parameters, which the embedding host can override.

Program files are read before returning the builder. Read failures report the
program ID, path, and original I/O error.

## Host responsibilities

This crate constructs an image; it does not open Engine, start application
services, or own process shutdown. The leader uses internal block pacing and
the verifier supplies replicated blocks. The leader replaces the builder's
default rent with rent fetched from the base chain before opening Engine;
the verifier uses the builder's default.

Using the same builder does not make separately supplied ELF files identical.
Leader and verifier deployments must provide matching program IDs and artifacts.
See the [configuration crate][config] and [deployment inputs][deployment].

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[config]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-config/README.md
[deployment]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/operations/deployment-prerequisites.md
