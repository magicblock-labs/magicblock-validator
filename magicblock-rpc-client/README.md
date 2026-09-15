# `magicblock-rpc-client`

Base-chain RPC client helpers for validator services: account and lookup-table
reads, transaction submission, and signature confirmation. This is not the
application-facing RPC server; that belongs to [Aperture][aperture].

## Submission versus confirmation

`MagicBlockSendTransactionConfig` selects send-only behavior or bounded waits
for processing and the client's requested commitment. The `ensure_processed`,
`ensure_committed`, and `ensure_processed_and_committed` constructors express
those different acknowledgement boundaries.

`MagicBlockSendTransactionOutcome` retains the signature and observed
processing/confirmation errors. Extracting a signature alone discards outcome
information; it does not prove successful execution. Confirmation may time out
after a transaction has landed, so callers must consider duplicate effects
before resending.

## Confirmation transport

Signature confirmation can use WebSocket subscriptions with a timed fallback
to coalesced HTTP status polling. Subscription connection failures, timeouts,
or stream termination do not by themselves establish transaction failure.
The configured commitment and deadline still determine what the caller awaits.

Account reads and ALT decoding remain RPC/data operations, not evidence that a
locally prepared settlement payload was accepted. See [settlement delivery][delivery]
for the caller's retry and completion responsibilities.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[aperture]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-aperture/README.md
[delivery]: https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/settlement-delivery.md
