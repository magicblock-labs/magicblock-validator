# `magicblock-program`

Native validator programs for settlement scheduling, repeated tasks, callbacks,
and sponsored ephemeral accounts. The [runtime image][runtime] installs their
entrypoints into Engine; this crate is not the base-chain Delegation Program.

## Execution boundaries

`magicblock_processor` provides the Magic, crank, callback, and ephemeral-system
entrypoints. Their public instruction definitions and IDs come from the
[Magic Program API][api].

The Magic program collects settlement intents through Magic Context and
coordinates them with host services. Scheduled tasks use the Engine
service-message path; the [task scheduler][scheduler] persists and cranks them.
Callback execution is separate from the base-chain action being reported.

Ephemeral-account operations manage local creation, resizing, closing, and
sponsor/rent accounting. Their authorization and accounting checks belong in
the execution path, not in an RPC-only precheck.

## Host integration

`init_magic_sys` installs the host adapter used by native execution to reach
Chainlink and settlement coordination. Build the shared runtime image before
opening Engine, then wire the leader's service integration before accepting
application work.

Use the shared instruction helpers for host-generated transactions. Preserve
signer provenance, writable-account checks, and atomic transaction failure when
changing native CPI behavior. See [account protection][protection] and
[settlement][settlement] for the wider contracts and their limitations.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[runtime]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-runtime/README.md
[api]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-magic-program-api/README.md
[scheduler]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-task-scheduler/README.md
[protection]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/account-lifecycle/account-protection.md
[settlement]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/account-lifecycle/settlement.md
