# `magicblock-committor-service`

Delivers the leader's settlement intents to the base chain: commits,
undelegations, finalization, and associated actions. Engine execution and local
intent scheduling are not themselves settlement completion.

## Delivery pipeline

`CommittorProcessor` prepares scheduled work for `IntentExecutionManager`.
The intent scheduler orders conflicting work before executors prepare and send
transactions; independent intents can execute concurrently under bounded permits.

Task construction resolves the account/delegation information and commit IDs
needed by the Delegation Program. Transaction preparation selects inline or
buffered payloads, prepares [lookup tables][tables] and [commit buffers][buffers],
and assembles the required transaction strategy. Large deliveries may require
separate staging and finalization phases.

`IntentExecutionService` integrates this pipeline with Chainlink, Engine,
recovered intents, and result handling. Base-chain action callbacks are separate
local transactions, not part of the base-chain transaction's atomic outcome.

## Retries and durability

Active executors and sleeping retries have separate limits. A retry releases its
execution permit during backoff so unrelated work can continue; retry-capacity
exhaustion makes that failure terminal.

Retry policy distinguishes commit-bearing intents, which have on-chain nonce
deduplication, from action-only intents. An unobserved successful send can make
an action-only retry execute twice, so those retries are restricted to pre-send
failures.

Optional persistence tracks intent status and supports recovery. It is not an
atomic transaction spanning the local database, base chain, and callbacks.
Completion reports distinguish intent execution from callback scheduling.
Callback signatures do not confirm callback execution; inspect execution errors
and scheduling errors rather than treating queue admission or a signature as
final success.

## Integration constraints

Preserve conflicting-account order, commit IDs, payload limits, ALT readiness,
and buffer cleanup across delivery changes. Retryability must reflect whether
a send could already have taken effect. See [settlement delivery][delivery] for
staging, acknowledgement, and recovery contracts.

[Workspace](https://github.com/magicblock-labs/magicblock-validator/blob/dev/README.md) · [Knowledge base](https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/README.md)

[tables]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-table-mania/README.md
[buffers]: https://github.com/magicblock-labs/magicblock-validator/blob/dev/magicblock-committor-program/README.md
[delivery]: https://github.com/magicblock-labs/knowledge-base/blob/main/projects/magicblock-validator/settlement-delivery.md
