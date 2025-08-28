#![allow(unused)]

use std::sync::atomic::{AtomicU16, Ordering};

use magicblock_config::RpcConfig;
use magicblock_core::link::accounts::LockedAccount;
use magicblock_gateway::{state::SharedState, JsonRpcServer};
use solana_account::{ReadableAccount, WritableAccount};
use solana_pubkey::Pubkey;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use test_kit::{guinea, ExecutionTestEnv, Signer};
use tokio_util::sync::CancellationToken;

pub const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

pub struct RpcTestEnv {
    pub execution: ExecutionTestEnv,
    pub rpc: RpcClient,
    pub pubsub: PubsubClient,
}

impl RpcTestEnv {
    pub async fn new() -> Self {
        const BLOCK_TIME_MS: u64 = 50;
        static PORT: AtomicU16 = AtomicU16::new(13001);
        let port = PORT.fetch_add(2, Ordering::Relaxed);
        let addr = "0.0.0.0".parse().unwrap();
        let config = RpcConfig { addr, port };
        let execution = ExecutionTestEnv::new();
        let state = SharedState::new(
            Pubkey::new_unique(),
            execution.accountsdb.clone(),
            execution.ledger.clone(),
            BLOCK_TIME_MS,
        );
        let cancel = CancellationToken::new();
        let rpc =
            JsonRpcServer::new(&config, state, &execution.dispatch, cancel)
                .await
                .expect(&format!(
                    "failed to start RPC service with: {config:?}"
                ));
        tokio::spawn(rpc.run());
        let rpc = RpcClient::new(format!("http://{addr}:{port}"));
        let pubsub = PubsubClient::new(&format!("ws://{addr}:{}", port + 1))
            .await
            .expect("failed to create a pubsub client to RPC server");
        Self {
            execution,
            rpc,
            pubsub,
        }
    }

    pub fn create_account(&self) -> LockedAccount {
        const SPACE: usize = 42;
        const LAMPORTS: u64 = 63;
        let pubkey = self
            .execution
            .create_account_with_config(LAMPORTS, SPACE, guinea::ID)
            .pubkey();
        let account = self.execution.accountsdb.get_account(&pubkey).unwrap();
        LockedAccount::new(pubkey, account)
    }

    pub fn create_token_account(
        &self,
        mint: Pubkey,
        owner: Pubkey,
    ) -> LockedAccount {
        if !self.execution.accountsdb.contains_account(&mint) {
            self.execution.fund_account(mint, 1);
            let mut mint_account =
                self.execution.accountsdb.get_account(&mint).unwrap();
            mint_account.resize(88, 0);
            mint_account.set_owner(TOKEN_PROGRAM_ID);
            mint_account.data_as_mut_slice()[40] = 9;
            self.execution
                .accountsdb
                .insert_account(&mint, &mint_account);
        }
        let token = self
            .execution
            .create_account_with_config(1, 165, TOKEN_PROGRAM_ID)
            .pubkey();
        let mut token_account =
            self.execution.accountsdb.get_account(&token).unwrap();
        token_account.data_as_mut_slice()[0..32]
            .copy_from_slice(&mint.to_bytes());
        token_account.data_as_mut_slice()[32..64]
            .copy_from_slice(&owner.as_ref());
        token_account.data_as_mut_slice()[73..105]
            .copy_from_slice(&owner.to_bytes());
        self.execution
            .accountsdb
            .insert_account(&token, &token_account);
        LockedAccount::new(token, token_account)
    }
}
