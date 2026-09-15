# `magicblock-services`

Leader-side services connecting base-chain outcomes to local Engine execution.
These services are distinct from transaction execution and from the settlement
delivery pipeline owned by the [committor][committor].

## Action callbacks

`ActionsCallbackService` builds callback transactions containing the base
action result and optional transaction signature. It takes the blockhash from
Engine, signs with the configured authority, and submits through its configured
RPC client in a background task. The callback program defines the callback
envelope and signer boundary.

Scheduling returns the constructed signatures before RPC submission completes;
later send failures are logged. These signatures are not callback execution
receipts. A callback is a separate execution from its base-chain action:
successful delivery on one side must not be interpreted as success on the other.

## Undelegation requests

`UndelegationRequestService` consumes Chainlink's observed requests and can
also poll for requests at the configured interval. A zero polling interval
disables polling, not the subscription path.

Before scheduling local work it checks account/request state through Chainlink.
Transient failures have bounded retries. A lagged broadcast receiver logs
skipped events; this is not a durable queue. The service participates in the
leader's coordinated shutdown.

See [settlement and callbacks][settlement] for the cross-chain contract.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[committor]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-committor-service/README.md
[settlement]: https://github.com/magicblock-labs/knowledge-base/blob/main/system/account-lifecycle/settlement.md
