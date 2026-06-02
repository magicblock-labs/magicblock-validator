# Post-Delegation Action Executor Plan

## Summary

Move post-delegation actions out of the Magic program and execute them from a
dedicated top-level executor program. The clone/final-continue instruction and
executor instruction must be adjacent siblings in the same transaction and must
mutually validate each other through the instructions sysvar.

This prevents the current reentrancy shape:

```text
Magic -> action program -> Magic
```

and replaces it with:

```text
PostActionExecutor -> action program -> Magic
```

## Key Changes

- Add a new builtin program id, separate from `CALLBACK_PROGRAM_ID`:

```rust
pub const POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID: Pubkey =
    pubkey!("PostAct111111111111111111111111111111111111");

pub enum PostDelegationActionInstruction {
    Execute {
        pubkey: Pubkey,
        actions: Vec<Instruction>,
    },
}
```

- Stop executing post-delegation actions inside `Magic::CloneAccount`.
- Keep actions attached to `CloneAccount` and final `CloneAccountContinue`, but
  use them only for validation.
- Keep the existing `PENDING_CLONES` design for chunked clones. It predates this
  fix and should not be rewritten as part of the post-action executor change.
- Add the instructions sysvar account to action-bearing `CloneAccount`, final
  `CloneAccountContinue`, and executor instructions.

Account cloner output:

```text
small:
[Magic::CloneAccount(actions), PostActionExecutor::Execute(actions)]

large:
[CloneAccountInit(target)]
[CloneAccountContinue(target, is_last=false)]*
[CloneAccountContinue(target, is_last=true, actions),
 PostActionExecutor::Execute(target, actions)]
```

## Validation Rules

Magic validates action-bearing `CloneAccount` and final
`CloneAccountContinue`:

- Target clone must produce a delegated account.
- Embedded actions must not use validator authority or effective validator
  authority as signer.
- The next top-level instruction from the instructions sysvar must be
  `PostActionExecutor::Execute`.
- The next executor instruction must contain the same `pubkey` and same
  `actions`.
- The executor must be the final top-level instruction in the transaction while
  `PENDING_CLONES` remains process-global state.

The executor validates before invoking actions:

- Current program id must be `POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID`.
- Executor must be top-level, not CPI; validate both stack height and the
  current instructions-sysvar entry.
- Validator authority must sign.
- Target account must be the delegated account at account index `1`.
- The previous top-level instruction from the instructions sysvar must be Magic
  `CloneAccount` or final `CloneAccountContinue`.
- Previous Magic instruction must contain the same `pubkey` and same `actions`.
- Previous clone must produce a delegated account.
- Invoke each action using `native_invoke(action, &signers)` from the executor
  program.

For final `CloneAccountContinue` with actions, Magic must not remove
`PENDING_CLONES`. The executor removes `PENDING_CLONES` only after all actions
succeed. This avoids the current process-global pending set being cleared when a
later sibling executor/action fails and transaction account state rolls back.

## Signer Injection

- The injected signer set is exactly the pubkeys flagged `is_signer` across the
  actions validated from the L1 payload. No other account may be escalated.
- On-curve keypairs and PDAs are both permitted in this set. The executor can do
  this because it is a builtin and passes the signer slice directly to
  `native_invoke`.
- Reject if validator authority or effective validator authority appears in the
  injected signer set.
- Trust model: the validator-signed clone/final-continue plus executor pair is trusted
  to carry only L1-validated payloads, matching the existing builtin signer
  injection pattern used by crank/callback-style execution.

## Test Plan

- Clone/final-continue with actions and no executor fails.
- Executor without immediately previous matching Magic instruction fails.
- Executor after non-matching pubkey/actions fails.
- Executor invoked through CPI fails.
- Successful small delegated clone executes actions once.
- Successful final chunk delegated clone executes actions once and clears
  `PENDING_CLONES`.
- Failed executor/action leaves the chunked clone pending for cleanup or retry.
- Replay of clone/final-continue plus executor fails because delegated accounts are
  not overridden.
- EATA projection validates against the projected ATA target, not the eATA.
- Regression transaction no longer fails with `ReentrancyNotAllowed`.

## Assumptions

- Delegated accounts are never overridden once cloned as delegated.
- Actions remain attached to delegated clone/final-continue instructions.
- The instructions sysvar is the source of truth for sibling instruction
  ordering.
- A new executor program id is preferred over reusing callback executor.
- `PENDING_CLONES` remains in scope as existing master behavior; this fix only
  changes when action-bearing final chunked clones clear it.
