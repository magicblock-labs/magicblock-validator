# magicblock-chainlink

Brings base-chain accounts and programs into the leader so applications can use
them locally. Chainlink fetches missing accounts, subscribes to remote changes,
and coordinates delegation and activation with Engine.

## How it fits

Account loading prepares the state needed for execution. Synchronization respects
local ownership: a remote update must not overwrite active delegated or ephemeral
state. Fetching an account is not, by itself, permission to modify it.

Delegation activation can include application actions and
[risk checks](../magicblock-aml/README.md). Failed activation may trigger a rescue
undelegation attempt; an attempt is not proof that undelegation has completed.

Engine owns local storage and execution. Use Chainlink's account-loading and
synchronization APIs rather than adding a separate remote-to-local write path.

## Configuration

Start with the remote providers and Chainlink settings in the
[leader configuration](../config.example.toml). Account loading depends on remote
RPC availability, and program loading needs the corresponding executable data.

For cross-chain behavior and recovery constraints, see
[activation and actions](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/activation-and-actions.md).

[Back to workspace](../README.md)
