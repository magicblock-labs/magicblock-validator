# Committor Service

Orchestrates commit/finalize/undelegate lifecycle for delegated accounts on Base layer.

## Architecture

### Task System (`src/tasks/`)

`BaseTaskImpl` is the central enum representing all task types:
```
BaseTaskImpl
├── Commit(CommitTask)       — commit account state/diff to base layer
├── Finalize(FinalizeTask)   — finalize a committed account
├── Undelegate(UndelegateTask) — undelegate an account
└── BaseAction(BaseActionTask) — call handler actions (V1/V2)
```

All variants implement `BaseTask` trait via delegation in `BaseTaskImpl`.
Domain logic lives in each type's own `impl` block, `BaseTaskImpl` just delegates.

### CommitTask (`src/tasks/commit_task.rs`)

Delivery strategy is an enum:
```
CommitDelivery
├── StateInArgs         — full account data in instruction args
├── StateInBuffer { stage } — data in on-chain buffer
├── DiffInArgs { base_account } — diff against base in args
└── DiffInBuffer { stage, base_account } — diff in buffer
```

`CommitBufferStage` tracks buffer lifecycle: `Preparation` -> `Cleanup`.

Key methods:
- `state_preparation_stage()` / `diff_preparation_stage()` — construct buffer preparation data
- `reset_commit_id()` — CommitTask-specific, not part of BaseTask trait
- `try_optimize_tx_size()` — switches from Args to Buffer delivery (deprecated, moving to strategist)

### BaseActionTask (`src/tasks/mod.rs`)

Enum with V1 (legacy call_handler) and V2 (call_handler_v2 with source_program).
`From` impls exist for `BaseActionTaskV1 -> BaseActionTask` and `BaseActionTaskV2 -> BaseActionTask`.

Each `BaseAction` may carry an optional `BaseActionCallback`. Callbacks are fired on L1
action failure or timeout — never delayed to intent completion. `BaseActionTask::extract_callback`
uses `take()`, so callbacks are consumed exactly once.

### Task Builder (`src/tasks/task_builder.rs`)

`TaskBuilderImpl` creates tasks from `ScheduledIntentBundle`:
- `commit_tasks()` — creates commit stage related tasks - fetches commit IDs and base accounts, creates CommitTask/BaseActionTask
- `finalize_tasks()` — creates finalize stage related tasks - creates Finalize/Undelegate/BaseAction tasks
- `create_action_tasks()` — dispatches V1 vs V2 based on `action.source_program`

### Task Strategist (`src/tasks/task_strategist.rs`)

Optimizes task layout for transaction size constraints. Uses `try_optimize_tx_size` on CommitTask
to switch from Args to Buffer strategy when data doesn't fit in instruction args.

`TransactionStrategy` also owns callback helpers:
- `has_actions_callbacks()` — whether any action task carries a callback
- `extract_action_callbacks()` — takes all callbacks out of action tasks (uses `take()`, safe to call twice)

### Transaction Flow

1. `TaskBuilderImpl` creates tasks from intent bundle
2. `TaskStrategist` optimizes task layout into `TransactionStrategy`
3. `DeliveryPreparator` prepares buffers (init, realloc, write chunks) and lookup tables
4. `TransactionPreparator` assembles final transaction from prepared tasks
5. `IntentExecutor` executes intent - (single/two stage) sends transactions to base layer, retries on defined failures by patching, propagates if error is not retriable. Manages action callback lifecycle on failure/timeout.

### Buffer Preparation (`PreparationTask`)

On-chain buffer lifecycle for large commits:
1. `init_instruction` — create buffer + chunks accounts
2. `realloc_instructions` — resize buffer if needed
3. `write_instructions` — write data chunks to buffer
4. After commit: `CleanupTask.instruction` closes buffer accounts

### Intent Executor (`src/intent_executor/`)

`IntentExecutorImpl<T, F, A>` is generic over `TransactionPreparator`, `TaskInfoFetcher`, and
`ActionsCallbackExecutor`. The `A` parameter is the callback executor — abstracted so
`IntentExecutorImpl` only knows *when* to fire callbacks, not how (could be channel, RPC, etc.).

Callback/timeout policy lives in `single_stage_execution_flow` and `two_stage_execution_flow`,
not inside the sub-executors. The sub-executors (`SingleStageExecutor`, `TwoStageExecutor`)
handle the mechanics of stripping actions and firing callbacks when an error occurs within
their execution loops, but the *timeout wrapping* is the responsibility of the flow methods.

**Action error / timeout rules:**
- Actions failed (stripped) → fire callbacks, continue without actions (if committed accounts exist)
- Timeout with callbacks → fire all callbacks, strip actions, continue
- Timeout without callbacks → continue unchanged
- Standalone actions (no committed accounts) fail → fire callbacks, intent fails

**`handle_actions_error` (free fn in `two_stage_executor.rs`, method in `SingleStageExecutor`)**:
Strips action tasks from a strategy via `remove_actions`, extracts + fires callbacks, returns
the stripped tasks as a cleanup strategy for junk. Safe to call multiple times (callbacks are `take()`n).

**`execute_callbacks()`** on each executor: convenience wrapper — calls `handle_actions_error`
and pushes the resulting cleanup strategy to `inner.junk`.

**Junk**: `IntentExecutorImpl.junk` collects `TransactionStrategy` values that need on-chain
cleanup (primarily buffer accounts for `CommitTask`). Always push the remaining strategy to
junk on both success AND error paths to ensure buffer cleanup.

**State machine (`TwoStageExecutor`)**: typestate pattern with `Initialized → Committed → Finalized`.
Transitions via `done(signature)`. `commit()` and `finalize()` return `IntentExecutorResult<Signature>`
and always `mem::take` their strategy into junk before returning.

### Key Types

- `TaskStrategy` — `Args | Buffer`
- `TransactionStrategy` — optimized tasks + lookup table keys
- `ActionsCallbackExecutor` — trait for firing `BaseActionCallback` values; impl determines mechanism

### Integration Tests (`test-integration/test-committor-service/`)

- `common.rs` — `TestFixture`, `create_commit_task`, `create_buffer_commit_task` helpers
- `test_delivery_preparator.rs` — buffer preparation, lookup tables, re-prepare scenarios
- `test_transaction_preparator.rs` — transaction assembly with various task combinations
- `test_intent_executor.rs` — end-to-end intent execution
