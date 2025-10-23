## Clients

- crate: `pubsub-client`

> A client for subscribing to messages from the RPC server.
> implements Solana WebSocket event subscriptions. [spec](https://solana.com/docs/rpc/websocket)

- crate: `quic-client`

> Simple client that connects to a given UDP port with the QUIC protocol and provides
> an interface for sending data which is restricted by the server's flow control.

- crate: `rpc-client`/`rpc-client-api`

> Software that interacts with the Solana blockchain, whether querying its
> state or submitting transactions, communicates with a Solana node over
> [JSON-RPC], using the [`RpcClient`] type.
>
> [JSON-RPC]: https://www.jsonrpc.org/specification

- crate: `tpu-client`

> Serialize and send transaction to the current and upcoming leader TPUs according
> Serialize and send a batch of transactions to the current and upcoming leader TPUs

### Client Tests

- `client-test/tests/client.rs:62` test_rpc_client
- `client-test/tests/client.rs:124` test_account_subscription
- `client-test/tests/client.rs:223` test_block_subscription
- `client-test/tests/client.rs:330` test_program_subscription

## RPC

- crate: `rpc`

> The `rpc` module implements the Solana RPC interface.

- includes `RpcService` which is instantiated directly by the `validator` crate
- listens on the `rpc_addr` staying alive via tokio runtime
- `JsonRpcRequestProcessor` `(rpc/src/rpc.rs:193)` wraps bank directly and implements
the `Metadata` trait

### Methods

#### Traits/API

Provides the below traits (omitting deprecated ones):

All those depend on the `[rpc]` macro of the `jsonrpc-derive` external crate.:

> Apply `#[rpc]` to a trait, and a `to_delegate` method is generated which
> wires up methods decorated with `#[rpc]` or `#[pubsub]` attributes.
> Attach the delegate to an `IoHandler` and the methods are now callable
> via JSON-RPC.

- `rpc_minimal::Minimal` (rpc/src/rpc.rs:2502)

> Minimal RPC interface that known validators are expected to provide

- `rpc_bank::BankData` (rpc/src/rpc.rs:2741)

> RPC interface that only depends on immediate Bank data
> Expected to be provided by API nodes

- `rpc_accounts::AccountsData` (rpc/src/rpc.rs:2956)

> RPC interface that depends on AccountsDB
> Expected to be provided by API nodes

- `rpc_accounts_scan` (rpc/src/rpc.rs:3109)

> RPC interface that depends on AccountsDB and requires accounts scan
> Expected to be provided by API nodes for now, but collected for easy separation and removal in
> the future.

- `rpc_full::Full` (rpc/src/rpc.rs:3271)

> Full RPC interface that an API node is expected to provide
> (rpc_minimal should also be provided by an API node)

#### Implementation

- provides implementations for each of the above traits that are based on a `Metadata` type
passed to each of the methods via the `meta` arg
- the `JsonRpcRequestProcessor` implements the `Metadata` requirements with lower level methods
- the implementation use the `meta` to _orchestrate_ resolving the RPC calls

### Pubsub

- `rpc/src/rpc_pubsub.rs:257` defines `RpcSolPubSubInternal` trait
- `rpc/src/rpc_pubsub.rs:422` implements it for `RpcSolPubSubImpl`
- `rpc/src/rpc_pubsub_service.rs:77` `PubSubService` spins up a thread to handle pubsub
requests

### TransactionStatusService

- implemented inside `rpc/src/transaction_status_service.rs` and spins up a thread
- instantiated by the validator

### RPC Test
- `rpc-test/tests/nonblocking.rs:17` test_tpu_send_transaction

- `rpc-test/tests/rpc.rs:70` test_rpc_send_tx
- `rpc-test/tests/rpc.rs:143` test_rpc_invalid_requests
- `rpc-test/tests/rpc.rs:176` test_rpc_slot_updates
- `rpc-test/tests/rpc.rs:244` test_rpc_subscriptions
- `rpc-test/tests/rpc.rs::511` test_tpu_send_transaction_with_quic

## Infrastructure

- crate: `streamer`

> The `streamer` module defines a set of services for efficiently pulling data from UDP sockets.
- includes a Quic module for both blocking and nonblocking implementations

## Details

### JsonRpcRequestProcessor

#### bank

- provided via `optimistically_confirmed_bank: Arc<RwLock<OptimisticallyConfirmedBank>>` field
if commitment is `confirmed`
- otherwise via `bank_forks: Arc<RwLock<BankForks>>` field to resolve the correct bank for the
  slot matching the commitment falling back to the `root_bank`

#### send_transaction

- `transaction_sender: Sender<TransactionInfo>` created on initialization and wrapped in
`Arc<Mutex<_>>` and the crossbeam-channel `Receiver` is returned
- `send-transaction-service` crate picks that up (see './send-transaction-service.md')

#### get_account_info

- `bank.get_account_info_with_commitment` is called with `commitment` and `min_context_slot`

