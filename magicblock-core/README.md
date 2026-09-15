# `magicblock-core`

Shared host-side types and utilities used across validator services: settlement
intent models, account traits, token-program helpers, and tracing setup.

The `intent` module carries scheduled work and results between the runtime,
committor, and callback services. These are coordination types, not a second
execution engine or a substitute for the public [Magic Program API][api].

## Logging

Use `logger` to initialize the process tracing configuration or test logging.
The `debug_panic!` macro panics in debug builds but logs in release builds;
it must not be used as validation for an expected failure.

Keep service-specific behavior in its owning crate. Engine owns transaction
execution, account storage, and replication.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[api]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-magic-program-api/README.md
