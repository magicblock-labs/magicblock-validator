use log::*;
use solana_rpc_client_api::config::RpcSimulateTransactionConfig;
use std::time::Duration;

use integration_test_tools::{
    conversions::stringify_simulation_result, IntegrationTestContext,
};
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signature},
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};

const VALIDATOR_WS: &str = "ws://127.0.0.1:8900";

pub struct PubSubEnv {
    /// Account we delegated into ephem
    pub account1: Keypair,
    /// Account we delegated into ephem
    pub account2: Keypair,
    /// Client to subscribe to account updates in ephem
    pub ws_client: PubsubClient,
    pub ctx: IntegrationTestContext,
}

impl PubSubEnv {
    pub async fn new() -> Self {
        let ctx = IntegrationTestContext::try_new().unwrap();

        let ws_client = PubsubClient::new(VALIDATOR_WS)
            .await
            .expect("failed to connect to ER validator via websocket");

        let payer_chain = Keypair::new();
        let account1 = Keypair::new();
        let account2 = Keypair::new();

        // Fund payer on chain which will fund accounts we delegate
        ctx.airdrop_chain(&payer_chain.pubkey(), 5 * LAMPORTS_PER_SOL)
            .unwrap();

        ctx.airdrop_chain_and_delegate(
            &payer_chain,
            &account1,
            LAMPORTS_PER_SOL,
        )
        .unwrap();
        ctx.airdrop_chain_and_delegate(
            &payer_chain,
            &account2,
            LAMPORTS_PER_SOL,
        )
        .unwrap();

        // wait for accounts to be fully written
        tokio::time::sleep(Duration::from_millis(50)).await;
        Self {
            ws_client,
            account1,
            account2,
            ctx,
        }
    }

    pub fn create_signed_transfer_tx(&self, lamports: u64) -> Transaction {
        let transfer_ix = system_instruction::transfer(
            &self.account1.pubkey(),
            &self.account2.pubkey(),
            lamports,
        );

        Transaction::new_signed_with_payer(
            &[transfer_ix],
            Some(&self.account1.pubkey()),
            &[&self.account1],
            self.ctx.try_get_latest_blockhash_ephem().unwrap(),
        )
    }

    pub fn send_signed_transaction(&self, tx: Transaction) -> Signature {
        let sig = tx.signatures[0];
        let res = self
            .ctx
            .try_ephem_client()
            .unwrap()
            .simulate_transaction_with_config(
                &tx,
                RpcSimulateTransactionConfig {
                    sig_verify: false,
                    replace_recent_blockhash: true,
                    ..Default::default()
                },
            )
            .unwrap();

        debug!("{}", stringify_simulation_result(res.value, &sig));

        self.ctx
            .try_ephem_client()
            .unwrap()
            .send_and_confirm_transaction(&tx)
            .unwrap()
    }

    pub fn transfer(&self, lamports: u64) -> Signature {
        let tx = self.create_signed_transfer_tx(lamports);
        self.send_signed_transaction(tx)
    }
}

#[macro_export]
macro_rules! drain_stream {
    ($rx:expr) => {{
        while let Ok(Some(_)) = ::tokio::time::timeout(
            ::std::time::Duration::from_millis(100),
            $rx.next(),
        )
        .await
        {}
    }};
}
