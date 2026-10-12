# ChainSync

Base-chain account synchronization for MagicBlock.

Confirmed HTTP snapshots preserve response slots and explicit account absence,
retrying transient failures without lowering the freshness floor.

WebSocket pools bound provider capacity, share acknowledged subscriptions, and
report lost subscriptions without restoring them on reconnect.

These components remain private until service assembly. The validator still uses Chainlink.
