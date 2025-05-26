use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{native_token::LAMPORTS_PER_SOL, signature::{Keypair, Signature}, signer::Signer,  system_transaction::transfer};

const OFFLINE_VALIDATOR_WS: &str = "ws://127.0.0.1:7800";
const OFFLINE_VALIDATOR_HTTP: &str = "http://127.0.0.1:7799";

pub struct PubSubEnv {
    pub ws_client: PubsubClient,
    pub rpc_client: RpcClient,
    pub account1: Keypair,
    pub account2: Keypair,
}

impl PubSubEnv {
    pub async fn new() -> Self {
        let ws_client = PubsubClient::new(OFFLINE_VALIDATOR_WS).await.expect("failed to connect to ER validator via websocket");
        let rpc_client = RpcClient::new(OFFLINE_VALIDATOR_HTTP.into());
        let account1 = Keypair::new();
        let account2 = Keypair::new();
        rpc_client.request_airdrop(&account1.pubkey(), LAMPORTS_PER_SOL).await.expect("failed to airdrop lamports to test account 1");
        rpc_client.request_airdrop(&account2.pubkey(), LAMPORTS_PER_SOL).await.expect("failed to airdrop lamports to test account 2");
        Self {
            rpc_client,
            ws_client,
            account1,
            account2,
        }
    }

    pub async fn transfer(&self, lamports: u64) -> Signature {
        let hash = self.rpc_client.get_latest_blockhash().await.expect("failed to get latest hash from ER");
        let txn = transfer(&self.account1, &self.account2.pubkey(), lamports, hash);
        self.rpc_client.send_transaction(&txn).await.expect("failed to send transaction")
    }
}
