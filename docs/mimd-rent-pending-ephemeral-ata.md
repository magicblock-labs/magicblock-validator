# Rent-Pending Ephemeral ATA Materialization

Status: draft for review.

This document specifies the validator-side protocol for a locally usable ATA
whose base-layer eATA does not exist yet. The user experience is "rent free" in
the ER, but base-layer eATA materialization still has explicit cost accounting.

## Problem

Today ATA/eATA commit assumes the companion eATA already exists on the base
layer and has the delegation metadata needed by the normal DLP commit flow.
That does not cover a gasless destination flow where tokens are sent to a
wallet before its base eATA exists.

The missing-base case must not be forced through the normal DLP commit path:
there is no delegated base eATA yet and therefore no normal DLP commit nonce,
delegation metadata, or rent reimbursement state for that account until e-token
materializes it and delegates it to this validator.

## Goals

- Allow a canonical ATA to exist and be writable in the ER before its base eATA
  exists.
- Define an explicit local creation path for rent-pending ATAs.
- Make the local token account explicitly recognizable as rent-pending.
- Keep local creation free to the user while keeping base materialization cost
  explicit.
- Support legacy SPL Token and Token-2022 account layouts.
- Preserve Token-2022 account size and extension requirements.
- Make base eATA creation and delegation to this validator idempotent and
  restart-recoverable.
- Reuse the existing commit/undelegation flow in the same base transaction
  after the idempotent eATA materialization instruction runs.
- Avoid weakening signer, delegation, account-sync, or SVM writable-account
  invariants.

## Non-Goals

- Do not make arbitrary ephemeral accounts commit to base.
- Do not create rent-pending ATAs through the generic ephemeral-account
  lifecycle.
- Do not treat every projected ATA as rent-pending.
- Do not synthesize a legacy SPL Token layout for Token-2022 accounts.
- Do not support native SOL / wrapped SOL in this path until a separate
  lamports-backed materialization design is approved.
- Do not use local account lamports as a rent-pending marker or close-safety
  mechanism.
- Do not add a separate empty/create-only materialization path in V1; this path
  exists to materialize eATA state needed by a normal commit or undelegation.
- Do not make local success look like base-layer completion before the
  committor confirms the base transaction containing both materialization and
  the normal commit/undelegation flow.

## Terms

- Base ATA: the canonical associated token account on Solana.
- eATA: the enhanced ATA PDA under `EATA_PROGRAM_ID`.
- Rent-pending ATA: a local canonical ATA that is writable in the ER and will be
  mapped to an eATA after the eATA is materialized on base.
- Local creation instruction: a Magic Program instruction that initializes the
  local canonical ATA in the ER without requiring a base-layer ATA/eATA to
  already exist.
- Materialization instruction: an idempotent e-token instruction, or ordered
  e-token instruction sequence, inserted into the same base transaction as the
  normal commit or undelegation instruction. It must ensure the eATA exists and
  is delegated to this validator before the DLP commit step for the affected
  eATA.

## Sentinel Close Authority

Rent-pending ATAs use the Solana rent sysvar as token-account close authority:

```rust
RENT_PENDING_ATA_CLOSE_AUTHORITY = solana_sysvar::rent::ID;
```

The rent sysvar is stable, easy to recognize, and not controlled by a normal
keypair. No validator, Magic Program, or e-token instruction may expose a path
that treats the rent sysvar as a signing authority for closing a local
rent-pending ATA. The local account must clear or replace the close authority
once it is no longer rent-pending.

`Pubkey::default()` remains the marker for ordinary projected ATAs that are
locally uncloseable. It must not be interpreted as rent-pending.

## Local Account Predicate

An account is a rent-pending ATA candidate only when all checks pass:

1. The local account pubkey is the canonical ATA for `(wallet_owner,
   token_program, mint)`.
2. The account owner is `TOKEN_PROGRAM_ID` or `TOKEN_2022_PROGRAM_ID`.
3. Token account data parses successfully for that token program.
4. Token account `owner` and `mint` match the canonical ATA derivation.
5. Token account `close_authority` is
   `Some(RENT_PENDING_ATA_CLOSE_AUTHORITY)`.
6. Local account flags have `delegated == true`.
7. Local account flags have `confined == false` and `undelegating == false`.
8. The account is not a native SOL / wrapped SOL token account.
9. The base eATA is missing, not delegated, or already delegated to this
   validator with compatible state. An eATA delegated to another validator is
   not a valid rent-pending target.

This predicate is sufficient to enter the rent-pending protocol path. It is not
the complete materialization authority. Scheduling must convert the candidate
into explicit eATA materialization metadata before the committor sees it.

V1 accepts only the delegated local token-account form. `delegated == true` is
the lifecycle flag that admits the account into this path; the `ephemeral` flag
is not an alternate admission path.

## Local Account Shape

For a non-native rent-pending ATA:

- `lamports` is not part of the rent-pending predicate.
- `remote_slot` is `0` while local-only.
- `delegated` is `true`.
- `ephemeral` is `false` in the V1 protocol.
- `owner` is the token program.
- token amount is the local balance to commit after eATA materialization.
- close authority is the rent-pending sentinel.

The local account is a token-program account, not a Magic Program ephemeral
account. The existing generic `ephemeral()` commit rejection stays in place.
The rent sysvar close authority makes the local ATA uncloseable by normal
users, so V1 does not need a zero-lamport local account convention for close
safety.

## Local Creation Instruction

Rent-pending ATAs are created locally through a dedicated Magic Program
instruction, not through the generic `CreateEphemeralAccount` path and not
through the base Associated Token Account Program.

Tentative API:

```rust
CreateRentPendingAta {
    wallet_owner: Pubkey,
    mint: Pubkey,
    token_program: Pubkey,
}
```

Accounts:

1. `[SIGNER]` payer or sponsor. This can be an on-curve signer or a PDA signer
   through CPI.
2. `[WRITE]` canonical ATA PDA to initialize locally.
3. `[]` mint account.
4. `[]` token program, either `TOKEN_PROGRAM_ID` or `TOKEN_2022_PROGRAM_ID`.

The recipient wallet owner is not required to sign. Unlike arbitrary
ephemeral-account creation, the account address is fixed by canonical ATA
derivation, and the initialized account owner is fixed to the token program.
The instruction may be invoked either top-level or by CPI.

The processor must:

1. Verify `token_program` is `TOKEN_PROGRAM_ID` or `TOKEN_2022_PROGRAM_ID`.
2. Verify the mint account is owned by `token_program` and is not the native
   SOL / wrapped SOL mint.
3. Derive the canonical ATA from `(wallet_owner, token_program, mint)` and
   require it equals the writable ATA account.
4. If the ATA account is locally empty/system-owned, initialize token-account
   data directly:
   - `mint = mint`;
   - `owner = wallet_owner`;
   - `amount = 0`;
   - token account state is initialized for SPL Token, and for Token-2022
     mirrors the mint `DefaultAccountState` extension (`Initialized` by
     default, `Frozen` when the mint default is frozen);
   - delegate is unset;
   - native state is unset;
   - close authority is `Some(RENT_PENDING_ATA_CLOSE_AUTHORITY)`.
5. Set local account metadata to the local rent-pending shape:
   - `owner = token_program`;
   - `delegated = true`;
   - `ephemeral = false`;
   - `confined = false`;
   - `undelegating = false`;
   - `remote_slot = 0`.
6. For Token-2022, compute the required token-account length from the mint
   extensions and reject required account extensions that cannot be initialized
   correctly in the ER. The implementation must not pack a Token-2022 account
   into the legacy SPL Token layout.

The instruction must be idempotent:

- existing matching rent-pending ATA: success, preserving token balance and
  existing account data except for re-normalizing the close-authority sentinel
  if needed;
- existing delegated/projected ATA that is not rent-pending: success without
  converting it to rent-pending;
- existing non-delegated, confined, undelegating, wrong-owner, wrong-mint, or
  wrong-token-program account: fail.

To avoid empty-account spam, V1 creation is only valid as part of a transaction
that actually gives the created rent-pending ATA a positive token balance. The
post-execution account verifier must reject and roll back any transaction that
creates a new rent-pending ATA and leaves it with `amount == 0`. Existing
matching rent-pending ATAs are not rejected merely because their current balance
is zero unless they were newly created in the same transaction.

No local creation charge is required in V1. Base materialization cost remains
charged when the account is committed or undelegated.

## Materialization Metadata

Scheduling must emit a dedicated metadata record for each rent-pending ATA while
also keeping the account in the normal committed-account set:

```rust
RentPendingAtaMaterialization {
    ata_pubkey: Pubkey,
    eata_pubkey: Pubkey,
    token_program: Pubkey,
    wallet_owner: Pubkey,
    mint: Pubkey,
    token_account_data_len: u64,
    validator: Pubkey,
    delegated_payer: Pubkey,
    delegated_vault: Pubkey,
}
```

The metadata is the authority for adding the same-transaction
ensure-created-and-delegated materialization instruction. The close-authority
sentinel is only the local candidate marker used to build this metadata.

The committed account itself remains the existing ATA account. The normal
`CommittedAccount::from_account_shared` ATA-to-eATA remap still produces the
eATA committed account and token amount used by the unchanged DLP commit flow
after materialization succeeds.

## Scheduling Behavior

When `ScheduleCommit`, `ScheduleCommitAndUndelegate`,
`ScheduleIntentBundle`, or equivalent scheduling code encounters a committed
account:

1. Before ATA-to-eATA remapping erases token account fields, check whether the
   account is a rent-pending ATA candidate.
2. If it is rent-pending, attach `RentPendingAtaMaterialization` metadata to the
   scheduled intent and still build the normal committed account for it. The
   normal committed account will remap ATA to eATA as it already does today.
3. Require the delegated payer path. The payer must be delegated and the magic
   fee vault must be present, writable, and delegated.
4. Charge the delegated payer and credit lamports to the delegated vault for the
   materialization cost in local state. Local ATA creation remains free; base
   eATA materialization is not free.
5. Reject duplicate references where the same final eATA appears more than once
   across the normal committed-account set and rent-pending metadata.

The existing signer checks stay unchanged:

- payer signer is still required;
- CPI parent/owner authority rules stay unchanged;
- validator-signed `AcceptScheduleCommits` stays required.

## Committor Behavior

The committor handles rent-pending metadata by adding materialization
instructions to the same base transaction that performs the normal commit or
undelegation:

1. Detect rent-pending materialization metadata while building the base commit
   or commit-and-undelegate transaction.
2. Build e-token idempotent eATA creation-plus-delegation instructions for all
   rent-pending metadata entries in the intent. The target validator is the
   validator recorded in the materialization metadata.
3. Insert those materialization instructions into the same transaction before
   the normal DLP commit/undelegate instruction for the affected eATA.
4. Append/build the existing commit/finalize/undelegate instructions unchanged
   after the materialization prefix.
5. Submit and confirm the single combined transaction with the same base-layer
   confirmation guarantees used by other committor tasks.

The desired V1 shape is atomic: if materialization fails, the normal
commit/undelegation does not execute; if commit/undelegation fails,
materialization is rolled back with the transaction. Retry resubmits the same
logical transaction and relies on both e-token creation and delegation being
idempotent.

If an existing committor builder currently requires confirmed eATA delegation
state before it can construct the DLP commit instruction, the implementation
should first move that dependency to deterministic account derivation or a
reusable e-token/API builder contract. A separate pre-confirmed
materialization transaction is a fallback only if the same-transaction shape is
blocked by transaction size, compute budget, or a hard DLP protocol constraint.

The e-token materialization instruction or instruction sequence must be
idempotent:

- missing eATA: create, initialize, and delegate it to this validator for
  `(wallet_owner, mint)`;
- existing matching eATA delegated to this validator: no-op success;
- existing matching eATA initialized but not delegated: delegate it to this
  validator;
- existing matching eATA delegated to another validator or with incompatible
  delegation metadata: fail with validator mismatch;
- existing mismatched eATA: fail with invalid base state.

An eATA may be created and delegated to another validator after the local
rent-pending ATA is created but before commit or undelegation is submitted.
That race is an expected base-state conflict: the combined transaction must
fail with validator mismatch, the normal commit/undelegation must not be
treated as successful, and the committor must surface the failure instead of
silently clearing local rent-pending state.

The combined materialization-plus-commit transaction must be persisted and
recoverable like other base-layer intent work. Retry must not create duplicate
state or double-debit the payer.

## e-token Instruction Contract

The validator must not fake eATA creation by writing through the DLP commit path
before the eATA exists. It needs an e-token/base-layer instruction with this
behavior:

1. Create the canonical eATA idempotently if it is missing.
2. Verify mint, wallet owner, eATA address, validator, and token program.
3. Idempotently delegate the eATA to this validator if it is initialized but not
   delegated.
4. Ensure the materialized eATA is in the exact state expected by the normal DLP
   commit path for this validator.
5. Fail if the eATA is already delegated to another validator or has
   incompatible delegation metadata.
6. Do not credit token amount directly. Token amount is committed by the normal
   ATA-to-eATA commit flow after materialization.
7. Reject native SOL / wrapped SOL accounts.
8. Reject malformed Token-2022 account state or unsupported required
   extensions.

If the existing e-token idempotent creation path can already leave the eATA
delegated to the requested validator, reuse it. Otherwise, add the smallest
e-token builder/API surface needed for the committor to create and delegate
idempotently inside the same transaction before the unchanged commit path.

The preferred reusable surfaces are:

- the Rust `ephemeral-spl-api` crate from
  `/Users/gabrielepicco/Documents/Solana/ephemeral-token/e-token-api`;
- existing Ephemeral Rollups SDK instruction/PDA builders, such as
  `deriveEphemeralAta` and `delegateEphemeralAtaIx`, where the committor-side
  integration is TypeScript-facing or can share that API contract.

The validator should not hand-roll e-token instruction data or PDA derivation
when one of those surfaces can own it. If a new materialization builder is
needed, it should live in the e-token API crate and/or SDK first, then be reused
by the committor.

## Chainlink and Projection Behavior

ATA/eATA projection remains unchanged for real base ATAs:

- projection requires a real base ATA layout;
- projection preserves token program owner, data length, and Token-2022
  extensions;
- projected ATAs use `close_authority = Some(Pubkey::default())`.

Rent-pending ATAs are different:

- a missing base ATA must not cause Chainlink to overwrite the local
  rent-pending ATA with an empty placeholder;
- a later base eATA update that matches a completed materialization may replace
  the local rent-pending state;
- a later mismatched base eATA update must mark the local state invalid or force
  a refresh instead of silently projecting over it.

## Invalid States

Reject or fail materialization for:

- sentinel close authority on a non-canonical ATA;
- sentinel close authority on an undelegated account;
- newly created rent-pending ATA that still has zero token balance at the end
  of its creation transaction;
- sentinel close authority on a confined or undelegating account;
- sentinel close authority on native SOL / wrapped SOL;
- token account data that does not parse for the declared token program;
- Token-2022 account data that would require legacy SPL packing;
- base eATA state that does not match the metadata;
- base eATA already delegated to a different validator.

## Security Notes

The sentinel close authority is not a signer grant. It is a data marker. The
protocol decision is:

```text
delegated local canonical ATA
+ rent-pending close-authority sentinel
+ structural token validation
=> build explicit rent-pending materialization metadata
```

Only the explicit metadata may drive committor materialization. This prevents a
locally forged close-authority field from bypassing payer/vault accounting,
base-state checks, idempotency, or token-program validation.

No path may relax:

- Magic Program payer signature checks;
- CPI parent/owner authorization;
- validator-signed acceptance of scheduled intents;
- SVM writable-account access validation;
- base-layer freshness checks;
- committor conflict scheduling and restart recovery.

## Validation Plan

Focused unit tests:

```bash
cargo test -p magicblock-core rent_pending --lib
cargo test -p magicblock-program rent_pending --lib
cargo test -p magicblock-committor-service rent_pending --lib
cargo test -p magicblock-chainlink rent_pending --lib
cargo test -p magicblock-processor rent_pending --test ephemeral_accounts
```

Required scenario coverage:

- local creation initializes a rent-pending ATA as `delegated == true` and
  `ephemeral == false`;
- creation transaction rolls back when a newly created rent-pending ATA ends
  with zero token balance;
- commit of a rent-pending ATA prepends idempotent eATA
  creation-plus-delegation to the same base transaction;
- undelegation of a rent-pending ATA prepends idempotent eATA
  creation-plus-delegation to the same base transaction;
- if the eATA is created after local rent-pending ATA creation and delegated to
  another validator before commit, commit fails with validator mismatch and
  does not clear local rent-pending state as successfully committed;
- if the eATA is created after local rent-pending ATA creation and delegated to
  another validator before undelegation, undelegation fails with validator
  mismatch and does not clear local rent-pending state as successfully
  undelegated.

Repository checks before merge:

```bash
make fmt
make lint
```

Integration coverage after implementation:

```bash
cd test-integration
make test-chainlink
make test-committor
make test-schedule-intents
```

The implementation handoff must report whether performance-sensitive paths were
measured or only reasoned about. The expected hot-path impact should be limited
to constant-time candidate detection during scheduling/projection plus a
same-transaction materialization instruction prefix for intents that actually
include rent-pending ATAs.

## Resolved V1 Decisions

1. V1 accepts only `delegated == true` local token accounts for this path.
2. V1 does not require a zero-lamport local ATA convention and does not add a
   separate empty/create-only materialization flow.
3. The e-token materialization instruction surface should be reused from
   `ephemeral-spl-api` and/or the Ephemeral Rollups SDK instead of being
   hand-rolled in the validator.
4. The preferred V1 committor shape is one combined base transaction: e-token
   idempotent creation-plus-delegation first, normal DLP commit/undelegation
   second.
5. Rent-pending ATAs use a dedicated Magic Program local creation instruction,
   stay `ephemeral == false`, and are valid only when the creation transaction
   leaves the new account with a positive token balance.
6. Materialization must be both idempotent creation and idempotent delegation to
   this validator; create-only materialization is insufficient.
