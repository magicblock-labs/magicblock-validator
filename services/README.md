# magicblock-services

Delivers base-chain action results as callbacks on the leader.

Callbacks report base-chain action results through a separate local transaction.
Scheduling a callback returns its signature before submission completes, so that
signature is not an execution receipt. Base-chain success and callback success
must be checked separately.

[ChainSync](../chainsync/README.md) owns delegation lifecycle monitoring and
undelegation requests. See the [committor service](../committor/README.md)
for settlement delivery.

[Back to workspace](../README.md)
