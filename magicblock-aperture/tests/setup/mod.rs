#![allow(dead_code)]

use std::sync::{Arc, OnceLock};

use engine::{Engine, testkit::TestEngine};
use keeper::testkit::{V42_ID, load_v42_lamports, signed_view, store_v42};
use magicblock_aperture::{SharedState, initialize_aperture};
use magicblock_chainlink::ProdChainlink;
use magicblock_config::config::aperture::ApertureConfig;
use magicblock_ledger_deprecated::Ledger;
use solana_account::{AccountBuilder, AccountMode, AccountSharedData};
use solana_instruction::Instruction;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;
use spl_token_2022::state::{Account as TokenAccount, AccountState, Mint};
use tokio_util::sync::CancellationToken;
use v42_calculator_interface::builder::Expr;

pub const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const PROGRAM_ID: Pubkey = V42_ID;
pub const REMOTE_ACCOUNT_CLAIMS_HEADER: &str = "X-MB-Remote-Account-Claims";

pub fn remote_account_claims_header(response: &reqwest::Response) -> u64 {
    response
        .headers()
        .get(REMOTE_ACCOUNT_CLAIMS_HEADER)
        .expect("remote account claims header should be present")
        .to_str()
        .expect("remote account claims header should be valid ASCII")
        .parse::<u64>()
        .expect("remote account claims header should be an integer")
}

pub fn transfer(from: Pubkey, to: Pubkey, amount: u64) -> Instruction {
    let delta = i64::try_from(amount).expect("test transfer fits i64");
    v42_calculator_interface::builder::transfer(from, to, delta)
}

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

fn chainlink(engine: &Engine) -> Arc<ProdChainlink> {
    Arc::new(
        ProdChainlink::try_new(engine.clone(), None).expect("create chainlink"),
    )
}

impl RpcTestEnv {
    pub const TRANSFER_AMOUNT: u64 = 1_000;
    pub const TOKEN_AMOUNT: u64 = 10_000_000_000;

    pub async fn new() -> Self {
        let engine = TestEngine::new().await;
        let inner: Engine = (*engine).clone();
        let state = SharedState::new(
            inner.clone(),
            shared_ledger(),
            chainlink(&inner),
            100,
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
        let sender = store_v42(&self.engine, 0, AccountMode::Ephemeral);
        let recipient = store_v42(&self.engine, 0, AccountMode::Ephemeral);
        let ix = transfer(sender, recipient, amount);
        (self.rpc_transaction(&[ix]), sender, recipient)
    }

    /// Executes the standard v42 write used by RPC history and notification tests.
    pub async fn execute_write(&self) -> Signature {
        let output = store_v42(&self.engine, 0, AccountMode::Ephemeral);
        let (signature, view) =
            signed_view(&self.engine, None, Expr::lit(42).compose(output, &[]));
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
        let sender = store_v42(&self.engine, 0, AccountMode::Ephemeral);
        let recipient = store_v42(&self.engine, 0, AccountMode::Ephemeral);
        let amount = load_v42_lamports(&self.engine, sender)
            .expect("stored balance")
            + 1;
        let (signature, view) = signed_view(
            &self.engine,
            None,
            transfer(sender, recipient, amount),
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

    pub fn create_token_account(&self, mint: Pubkey, owner: Pubkey) -> Pubkey {
        if self.engine.account(mint).is_none() {
            let mut data = vec![0; Mint::LEN];
            Mint::pack(
                Mint {
                    mint_authority: None.into(),
                    supply: Self::TOKEN_AMOUNT,
                    decimals: 9,
                    is_initialized: true,
                    freeze_authority: None.into(),
                },
                &mut data,
            )
            .expect("pack token mint");
            self.store(mint, TOKEN_PROGRAM_ID, data);
        }

        let mut data = vec![0; TokenAccount::LEN];
        TokenAccount::pack(
            TokenAccount {
                mint,
                owner,
                amount: Self::TOKEN_AMOUNT,
                delegate: Some(owner).into(),
                state: AccountState::Initialized,
                is_native: None.into(),
                delegated_amount: Self::TOKEN_AMOUNT,
                close_authority: None.into(),
            },
            &mut data,
        )
        .expect("pack token account");
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
