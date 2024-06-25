use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    native_token::LAMPORTS_PER_SOL,
    signature::Keypair,
    signer::{SeedDerivable, Signer},
};

pub struct TriggerCommitTestContext {
    pub payer: Keypair,
    pub committee: Keypair,
    pub commitment: CommitmentConfig,
    pub client: RpcClient,
    pub blockhash: Hash,
}

impl TriggerCommitTestContext {
    pub fn new() -> Self {
        let payer = Keypair::from_seed(&[2u8; 32]).unwrap();
        let committee = Keypair::from_seed(&[3u8; 32]).unwrap();
        let commitment = CommitmentConfig::processed();

        let client = RpcClient::new_with_commitment(
            "http://localhost:8899".to_string(),
            commitment,
        );
        client
            .request_airdrop(&payer.pubkey(), LAMPORTS_PER_SOL * 100)
            .unwrap();
        // Account needs to exist to be commitable
        client
            .request_airdrop(&committee.pubkey(), LAMPORTS_PER_SOL)
            .unwrap();

        let blockhash = client.get_latest_blockhash().unwrap();

        Self {
            payer,
            committee,
            commitment,
            client,
            blockhash,
        }
    }
}
