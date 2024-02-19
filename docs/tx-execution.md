# Transaction Execution

- entry point: `svm/src/transaction_processor.rs`

## MessageProcessor

- _runs_ programs inside the VM `program-runtime/src/message_processor.rs:45`

## LoadedPrograms

- defined in `program-runtime/src/loaded_programs.rs:573`
- important from the below:
  - **keeps the compiled executables around**, mb validator should keep all of them
  - TODO: how do we deal with programs that are upgraded on main-net while our node is
    running?

> - is validator global and fork graph aware, so it can optimize the commonalities across banks.
> - handles the visibility rules of un/re/deployments.
> - stores the usage statistics and verification status of each program.
> - is elastic and uses a probabilistic eviction stragety based on the usage statistics.
> - also keeps the compiled executables around, but only for the most used programs.
> - supports various kinds of tombstones to avoid loading programs which can not be loaded.
> - cleans up entries on orphan branches when the block store is rerooted.
> - supports the recompilation phase before feature activations which can change cached programs.
> - manages the environments of the programs and upcoming environments for the next epoch.
> - allows for cooperative loading of TX batches which hit the same missing programs simultaneously.
> - enforces that all programs used in a batch are eagerly loaded ahead of execution.
> - is not persisted to disk or a snapshot, so it needs to cold start and warm up first.
