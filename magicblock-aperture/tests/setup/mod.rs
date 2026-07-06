#![allow(dead_code)]

use std::sync::{Arc, OnceLock};

use engine::{Engine, testkit::TestEngine};
use keeper::testkit::V42_ID;
use magicblock_aperture::{
    initialize_aperture,
    state::{ChainlinkImpl, InnerChainlinkImpl, NodeContext, SharedState},
};
use magicblock_config::config::aperture::ApertureConfig;
use magicblock_ledger_deprecated::Ledger;
use solana_account::{AccountBuilder, AccountMode, AccountSharedData};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio_util::sync::CancellationToken;
use v42_calculator_interface::builder::Expr;

pub const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const PROGRAM_ID: Pubkey = V42_ID;

pub struct RpcTestEnv {
    pub engine: TestEngine,
    pub rpc: RpcClient,
    pub pubsub: PubsubClient,
    cancel: CancellationToken,
}

fn shared_ledger() -> Arc<Ledger> {
    static SHARED: OnceLock<Arc<Ledger>> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            let dir = keeper::testkit::tempdir();
            let ledger = Ledger::open(dir.path()).expect("open test ledger");
            std::mem::forget(dir);
            Arc::new(ledger)
        })
        .clone()
}

fn chainlink(engine: &Engine) -> Arc<ChainlinkImpl> {
    let bank = Arc::new(engine.clone());
    let inner =
        InnerChainlinkImpl::try_new(&bank, None).expect("create chainlink");
    Arc::new(ChainlinkImpl::enabled(inner))
}

impl RpcTestEnv {
    pub const BASE_FEE: u64 = 5_000;
    pub const TRANSFER_AMOUNT: u64 = 1_000;
    pub const TOKEN_AMOUNT: u64 = 10_000_000_000;

    pub async fn new() -> Self {
        let engine = TestEngine::new().await;
        let inner: Engine = (*engine).clone();
        let state = SharedState::new(
            NodeContext {
                identity: inner.authority(),
                is_primary: true,
                base_fee: Self::BASE_FEE,
                featureset: Default::default(),
                blocktime: 100,
            },
            inner.clone(),
            shared_ledger(),
            chainlink(&inner),
        );
        let cancel = CancellationToken::new();
        let server = initialize_aperture(
            &ApertureConfig {
                listen: "127.0.0.1:0".parse().expect("test listen address"),
                ..Default::default()
            },
            state,
            cancel.clone(),
        )
        .await
        .expect("initialize aperture test server");
        let rpc_url = format!("http://{}", server.http_addr());
        let pubsub_url = format!("ws://{}", server.ws_addr());
        tokio::spawn(server.run());

        Self {
            engine,
            rpc: RpcClient::new(rpc_url),
            pubsub: PubsubClient::new(&pubsub_url)
                .await
                .expect("connect to aperture pubsub"),
            cancel,
        }
    }

    /// Builds the client-side Solana envelope used at the JSON-RPC boundary.
    pub fn rpc_transaction(&self, ixs: &[Instruction]) -> Transaction {
        let payer = self.engine.signer();
        Transaction::new_signed_with_payer(
            ixs,
            Some(&payer.pubkey()),
            &[payer],
            self.engine.blockhash(),
        )
    }

    /// Creates two v42 accounts and a client-side transfer between them.
    pub fn rpc_transfer(&self, amount: u64) -> (Transaction, Pubkey, Pubkey) {
        let sender = self.engine.store_v42(0, AccountMode::Ephemeral);
        let recipient = self.engine.store_v42(0, AccountMode::Ephemeral);
        let ix = v42_calculator_interface::builder::transfer(
            sender, recipient, amount,
        );
        (self.rpc_transaction(&[ix]), sender, recipient)
    }

    /// Executes the standard v42 write used by RPC history and notification tests.
    pub async fn execute_write(&self) -> Signature {
        let output = self.engine.store_v42(0, AccountMode::Ephemeral);
        let (signature, view) = self
            .engine
            .signed_view(None, Expr::lit(42).compose(output, &[]));
        self.engine
            .transaction(view)
            .expect("compose transaction view")
            .execute()
            .await
            .expect("engine available")
            .expect("v42 write succeeds");
        signature
    }

    pub async fn execute_failing_transfer(&self) -> Signature {
        let sender = self.engine.store_v42(0, AccountMode::Ephemeral);
        let recipient = self.engine.store_v42(0, AccountMode::Ephemeral);
        let amount = self
            .engine
            .load_v42_lamports(sender)
            .expect("stored balance")
            + 1;
        let (signature, view) = self.engine.signed_view(
            None,
            v42_calculator_interface::builder::transfer(
                sender, recipient, amount,
            ),
        );
        let result = self
            .engine
            .transaction(view)
            .expect("compose transaction view")
            .execute()
            .await
            .expect("engine available");
        assert!(result.is_err(), "underfunded transfer must fail");
        signature
    }

    /// Stores the exact SPL layouts exercised by the token RPC methods.
    pub fn create_token_account(&self, mint: Pubkey, owner: Pubkey) -> Pubkey {
        const MINT_DECIMALS_OFFSET: usize = 44;
        const MINT_DATA_LEN: usize = 88;
        const TOKEN_ACCOUNT_DATA_LEN: usize = 165;

        if self.engine.account(mint).is_none() {
            let mut data = vec![0; MINT_DATA_LEN];
            data[MINT_DECIMALS_OFFSET] = 9;
            self.store(mint, TOKEN_PROGRAM_ID, data);
        }

        let mut data = vec![0; TOKEN_ACCOUNT_DATA_LEN];
        data[..32].copy_from_slice(mint.as_ref());
        data[32..64].copy_from_slice(owner.as_ref());
        data[64..72].copy_from_slice(&Self::TOKEN_AMOUNT.to_le_bytes());
        data[76..108].copy_from_slice(owner.as_ref());
        let key = Pubkey::new_unique();
        self.store(key, TOKEN_PROGRAM_ID, data);
        key
    }

    fn store(&self, key: Pubkey, owner: Pubkey, data: Vec<u8>) {
        let account: AccountSharedData = AccountBuilder::default()
            .lamports(Self::TOKEN_AMOUNT)
            .owner(owner)
            .data(data)
            .mode(AccountMode::Ephemeral)
            .build();
        self.engine
            .accounts()
            .store(&[(key, account)])
            .expect("store test account");
    }
}

impl Drop for RpcTestEnv {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
