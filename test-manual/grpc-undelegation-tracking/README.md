# gRPC Undelegation Tracking Test

## What does it do

This test validates that the validator's gRPC subscription correctly tracks undelegation events
on devnet. It uses the FlexiCounter program to delegate, modify, and undelegate accounts while
verifying that the validator detects these state changes via Prometheus metrics.

The test covers two undelegation scenarios:

### Normal Flow
1. Delegate a counter account to the validator
2. Clone the account to the ephemeral validator
3. Increment the counter on ephemeral
4. Undelegate via ephemeral (triggers both account subscription and pubkey tracking)

### Cheating Flow
1. Delegate the counter account again
2. **Never** clone it to ephemeral
3. Directly undelegate on devnet using commit+finalize+undelegate instructions
4. This bypasses ephemeral entirely and only triggers pubkey tracking via delegation record watch

The test verifies both flows are detected via Prometheus metrics:
- `transaction_subscription_pubkey_updates_count` increases by at least 2 (both undelegations)
- `transaction_subscription_account_updates_count` increases by at least 1 (only the cloned account)

## Module Structure

| Module | Description |
|--------|-------------|
| `main.rs` | Test orchestration with 10 phases: setup, init counter, record metrics, delegate, clone+increment, undelegate via ephemeral, delegate again, direct undelegate on devnet, verify metrics, verify final state |
| `counter_sdk.rs` | FlexiCounter program SDK (`f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4`) with PDA derivation and instruction builders for Init, Delegate, AddAndScheduleCommit |
| `delegation_sdk.rs` | Direct delegation program interaction with DelegationRecord/DelegationMetadata deserialization and instruction builders for commit_state, finalize, undelegate |
| `metrics.rs` | Prometheus metrics fetching and parsing from `http://127.0.0.1:9000/metrics` |
| `test_utils.rs` | RPC client creation, keypair loading, transaction send/confirm utilities |

## Why can it not be fully automated

The test cannot be fully automated because it requires:

- Access to a Helius API key for connecting to their devnet RPC
- A Solana keypair on devnet with sufficient SOL for transaction fees
- A running ephemeral validator configured to track devnet delegations
- The hardcoded validator keypair (`mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev`) must be authorized

## How to run it

1. Set your Helius API key:
   ```bash
   export HELIUS_API_KEY=your_helius_api_key_here
   ```

2. Set your funded devnet keypair path:
   ```bash
   export KEYPAIR_PATH=~/.config/solana/id.json
   ```

3. Run the test from the `test-manual` directory:
   ```bash
   make test-grpc-undelegation-tracking
   ```

This will automatically:
- Airdrop SOL to your keypair (if needed)
- Create a temporary config for devnet gRPC
- Start the ephemeral validator in background
- Run the test
- Clean up the validator process

## Test Phases

| Phase | Description |
|-------|-------------|
| 0 | Setup connections and keypairs |
| 1 | Initialize counter on devnet (if needed) |
| 2 | Record initial Prometheus metrics |
| 3 | Delegate counter to validator |
| 4 | Clone and increment counter on ephemeral |
| 5 | Undelegate via ephemeral (normal flow) |
| 6 | Verify counter updated on devnet |
| 7 | Delegate counter again for cheating flow |
| 8 | Direct undelegation on devnet (cheating flow) |
| 9 | Verify metrics show both undelegations detected |
| 10 | Verify final counter state and metadata cleanup |

## Expected Output

On success, you'll see:
```
✓ Counter value correctly updated on devnet
✓ Pubkey updates tracking verified
✓ Account updates tracking verified
✓ Delegation metadata correctly closed
✓ All assertions passed!
✓ Test completed successfully!
```