#![allow(unused)]

use std::{
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc,
    },
    thread,
};

use magicblock_accounts_db::AccountsDb;
use magicblock_aperture::{
    state::{ChainlinkImpl, NodeContext, SharedState},
    JsonRpcServer,
};
use magicblock_config::RpcConfig;
use magicblock_core::link::accounts::LockedAccount;
use magicblock_core::traits::AccountsBank;
use magicblock_core::Slot;
use magicblock_ledger::LatestBlock;
use solana_account::{ReadableAccount, WritableAccount};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_transaction::Transaction;
use test_kit::{
    guinea::{self, GuineaInstruction},
    AccountMeta, ExecutionTestEnv, Instruction, Signer,
};
use tokio_util::sync::CancellationToken;

pub const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

/// An end-to-end integration testing environment for the RPC server.
///
/// This struct bundles a simulated validator backend (`ExecutionTestEnv`) with a live,
/// running `JsonRpcServer` and connected `RpcClient` and `PubsubClient` instances.
/// It provides a comprehensive harness for writing tests that interact with the
/// RPC API as a real client would.
pub struct RpcTestEnv {
    /// The simulated validator backend, containing the `AccountsDb` and `Ledger`.
    pub execution: ExecutionTestEnv,
    /// A connected RPC client for sending requests to the test server.
    pub rpc: RpcClient,
    /// A connected Pub/Sub client for WebSocket tests.
    pub pubsub: PubsubClient,
    /// A handle to the latest block information in the ledger.
    pub block: LatestBlock,
}

fn chainlink(accounts_db: &Arc<AccountsDb>) -> ChainlinkImpl {
    ChainlinkImpl::try_new(accounts_db, None)
        .expect("Failed to create Chainlink")
}

impl RpcTestEnv {
    // --- Constants ---
    pub const BASE_FEE: u64 = ExecutionTestEnv::BASE_FEE;
    pub const INIT_ACCOUNT_BALANCE: u64 = 10_000_000_000;
    pub const TRANSFER_AMOUNT: u64 = 1000;

    /// Creates a new, fully initialized RPC test environment.
    ///
    /// This function sets up a complete, self-contained testing stack:
    /// 1.  Initializes a simulated validator backend (`ExecutionTestEnv`).
    /// 2.  Selects a unique network port to avoid conflicts during parallel test runs.
    /// 3.  Starts a live `JsonRpcServer` (HTTP and WebSocket) in a background task.
    /// 4.  Connects an `RpcClient` and `PubsubClient` to the running server.
    pub async fn new() -> Self {
        const BLOCK_TIME_MS: u64 = 50;
        static PORT: AtomicU16 = AtomicU16::new(13001);

        let execution = ExecutionTestEnv::new();

        // Use an atomic counter to ensure each test instance gets a unique port.
        let port = PORT.fetch_add(2, Ordering::Relaxed);
        let addr = "0.0.0.0".parse().unwrap();
        let config = RpcConfig { addr, port };

        let faucet = Keypair::new();
        execution.fund_account(faucet.pubkey(), Self::INIT_ACCOUNT_BALANCE);

        let node_context = NodeContext {
            identity: execution.payer.pubkey(),
            faucet: Some(faucet),
            base_fee: Self::BASE_FEE,
            featureset: Default::default(),
        };
        let state = SharedState::new(
            node_context,
            execution.accountsdb.clone(),
            execution.ledger.clone(),
            chainlink(&execution.accountsdb),
            BLOCK_TIME_MS,
        );
        let cancel = CancellationToken::new();

        let rpc_server =
            JsonRpcServer::new(&config, state, &execution.dispatch, cancel)
                .await
                .unwrap_or_else(|e| {
                    panic!(
                        "failed to start RPC service with config {:?}: {}",
                        config, e
                    )
                });

        tokio::spawn(rpc_server.run());

        let rpc_url = format!("http://{addr}:{port}");
        let pubsub_url = format!("ws://{addr}:{}", port + 1);

        let rpc = RpcClient::new(rpc_url);
        let pubsub = PubsubClient::new(&pubsub_url)
            .await
            .expect("failed to create a pubsub client to RPC server");

        // Allow server threads to initialize.
        thread::yield_now();

        Self {
            block: execution.ledger.latest_block().clone(),
            execution,
            rpc,
            pubsub,
        }
    }

    // --- Account Creation Helpers ---

    /// Creates a standard account with the default initial balance and owner.
    pub fn create_account(&self) -> LockedAccount {
        const SPACE: usize = 42;
        let pubkey = self
            .execution
            .create_account_with_config(
                Self::INIT_ACCOUNT_BALANCE,
                SPACE,
                guinea::ID,
            )
            .pubkey();
        let account = self.execution.accountsdb.get_account(&pubkey).unwrap();
        LockedAccount::new(pubkey, account)
    }

    /// Creates a mock SPL Token account with the specified mint and owner.
    pub fn create_token_account(
        &self,
        mint: Pubkey,
        owner: Pubkey,
    ) -> LockedAccount {
        // Define SPL Token account layout constants.
        const MINT_OFFSET: usize = 0;
        const OWNER_OFFSET: usize = 32;
        const AMOUNT_OFFSET: usize = 64;
        const DELEGATE_OFFSET: usize = 76;
        const MINT_DECIMALS_OFFSET: usize = 40;
        const MINT_DATA_LEN: usize = 88;
        const TOKEN_ACCOUNT_DATA_LEN: usize = 165;

        // Create and configure the mint account if it doesn't exist.
        if !self.execution.accountsdb.contains_account(&mint) {
            self.execution
                .fund_account(mint, Self::INIT_ACCOUNT_BALANCE);
            let mut mint_account =
                self.execution.accountsdb.get_account(&mint).unwrap();
            mint_account.resize(MINT_DATA_LEN, 0);
            mint_account.set_owner(TOKEN_PROGRAM_ID);
            // Set mint decimals to 9.
            mint_account.data_as_mut_slice()[MINT_DECIMALS_OFFSET] = 9;
            self.execution
                .accountsdb
                .insert_account(&mint, &mint_account);
        }

        // Create the token account itself.
        let token_pubkey = self
            .execution
            .create_account_with_config(
                Self::INIT_ACCOUNT_BALANCE,
                TOKEN_ACCOUNT_DATA_LEN,
                TOKEN_PROGRAM_ID,
            )
            .pubkey();

        // Manually write the SPL Token state into the account's data buffer.
        let mut token_account = self
            .execution
            .accountsdb
            .get_account(&token_pubkey)
            .unwrap();
        let data = token_account.data_as_mut_slice();
        data[MINT_OFFSET..MINT_OFFSET + 32].copy_from_slice(&mint.to_bytes());
        data[OWNER_OFFSET..OWNER_OFFSET + 32].copy_from_slice(owner.as_ref());
        data[AMOUNT_OFFSET..AMOUNT_OFFSET + 8]
            .copy_from_slice(&Self::INIT_ACCOUNT_BALANCE.to_le_bytes());
        data[DELEGATE_OFFSET..DELEGATE_OFFSET + 32]
            .copy_from_slice(&owner.to_bytes());

        self.execution
            .accountsdb
            .insert_account(&token_pubkey, &token_account);
        LockedAccount::new(token_pubkey, token_account)
    }

    /// Advances the ledger by the specified number of slots.
    pub fn advance_slots(&self, count: usize) {
        for _ in 0..count {
            self.execution.advance_slot();
        }
    }

    /// Returns the latest slot number from the ledger.
    pub fn latest_slot(&self) -> Slot {
        self.block.load().slot
    }

    /// Creates and executes a generic transaction that modifies a new account.
    pub async fn execute_transaction(&self) -> Signature {
        let account = self.create_account();
        let ix = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::WriteByteToData(42),
            vec![AccountMeta::new(account.pubkey, false)],
        );
        let txn = self.execution.build_transaction(&[ix]);
        let signature = txn.signatures[0];
        self.execution
            .execute_transaction(txn)
            .await
            .expect("failed to execute modifying transaction");
        signature
    }

    /// Creates and executes transaction to transfer some lamports to account
    pub async fn transfer_lamports(&self, recipient: Pubkey, lamports: u64) {
        let txn = self.build_transfer_txn_with_params(
            Pubkey::new_unique(),
            recipient,
            false,
        );
        self.execution
            .transaction_scheduler
            .execute(txn)
            .await
            .unwrap();
    }

    /// Builds a transfer transaction between two new, randomly generated accounts.
    pub fn build_transfer_txn(&self) -> Transaction {
        let from = Pubkey::new_unique();
        let to = Pubkey::new_unique();
        self.build_transfer_txn_with_params(from, to, false)
    }

    /// Builds a transfer transaction that is guaranteed to fail due to insufficient funds.
    pub fn build_failing_transfer_txn(&self) -> Transaction {
        let from = Pubkey::new_unique();
        let to = Pubkey::new_unique();
        self.build_transfer_txn_with_params(from, to, true)
    }

    /// A generic helper to build a transfer transaction with specific parameters.
    /// If `fail` is true, the `from` account is created with insufficient funds.
    pub fn build_transfer_txn_with_params(
        &self,
        from: Pubkey,
        to: Pubkey,
        fail: bool,
    ) -> Transaction {
        let from_lamports = if fail {
            1 // Not enough to cover the transfer amount
        } else {
            Self::INIT_ACCOUNT_BALANCE
        };
        self.execution
            .fund_account_with_owner(from, from_lamports, guinea::ID);
        self.execution.fund_account_with_owner(
            to,
            Self::INIT_ACCOUNT_BALANCE,
            guinea::ID,
        );
        let ix = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::Transfer(Self::TRANSFER_AMOUNT),
            vec![AccountMeta::new(from, false), AccountMeta::new(to, false)],
        );
        self.execution.build_transaction(&[ix])
    }
}
