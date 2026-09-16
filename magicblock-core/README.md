# magicblock-core

Shared types and utilities for validator services, including settlement intents,
account helpers, and logging setup.

Use this crate for concepts shared across services. Keep service-specific
behavior in the owning crate; transaction execution and storage belong to
Engine. Application instruction definitions live in the
[Magic Program API](../magicblock-magic-program-api/README.md).

## Logging

The `logger` module sets up process or test logging. The `debug_panic!` macro
panics only in debug builds and logs in release builds, so it is not a substitute
for handling expected errors.

[Back to workspace](../README.md)
