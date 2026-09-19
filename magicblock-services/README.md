# magicblock-services

Connects base-chain activity to local execution on the leader. It delivers
action results as callbacks and handles observed undelegation requests.

## Callbacks

Callbacks report base-chain action results through a separate local transaction.
Scheduling a callback returns its signature before submission completes, so that
signature is not an execution receipt. Base-chain success and callback success
must be checked separately.

## Undelegation requests

Requests arrive through Chainlink subscriptions and optional polling. Disabling
polling does not disable subscriptions. The service checks account state before
scheduling work and retries transient failures, but the subscription channel is
not a durable queue.

See the [committor service](../magicblock-committor-service/README.md) for settlement
delivery and the [leader configuration](../config.example.toml) for settings.

[Back to workspace](../README.md)
