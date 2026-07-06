use std::sync::Arc;

use anyhow::{Context, Result, bail};
use borsh::BorshDeserialize;
use mdp::{
    ID,
    consts::ER_RECORD_SEED,
    instructions::{sync::SyncInstruction, version::v0::SyncRecordV0},
    state::record::ErRecord,
};
use solana_account::ReadableAccount;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk_ids::system_program;
use solana_signer::Signer;
use solana_transaction::Transaction;

pub struct DomainClient {
    client: Arc<RpcClient>,
}

impl DomainClient {
    pub fn new(url: impl ToString) -> Self {
        Self {
            client: Arc::new(RpcClient::new_with_commitment(
                url.to_string(),
                CommitmentConfig::confirmed(),
            )),
        }
    }

    pub async fn register(
        &self,
        payer: &Keypair,
        record: ErRecord,
    ) -> Result<()> {
        self.send(
            payer,
            record.pda().0,
            mdp::instructions::Instruction::Register(record),
        )
        .await
        .context("failed to register domain record")
    }

    pub async fn sync(&self, payer: &Keypair, record: &ErRecord) -> Result<()> {
        let update = SyncRecordV0 {
            identity: *record.identity(),
            status: Some(record.status()),
            block_time_ms: Some(record.block_time_ms()),
            base_fee: Some(record.base_fee()),
            features: Some(record.features().clone()),
            load_average: Some(record.load_average()),
            country_code: Some(record.country_code()),
            addr: Some(record.addr().to_owned()),
        };
        self.send(
            payer,
            record.pda().0,
            mdp::instructions::Instruction::Sync(SyncInstruction::V0(update)),
        )
        .await
        .context("failed to synchronize domain record")
    }

    pub async fn unregister(&self, payer: &Keypair) -> Result<()> {
        let pda = record_pda(&payer.pubkey());
        if self.fetch(&pda).await?.is_none() {
            bail!("no domain record exists for {}", payer.pubkey());
        }
        self.send(
            payer,
            pda,
            mdp::instructions::Instruction::Unregister(payer.pubkey()),
        )
        .await
        .context("failed to unregister domain record")
    }

    async fn fetch(&self, pubkey: &Pubkey) -> Result<Option<ErRecord>> {
        let response = self
            .client
            .get_account_with_commitment(pubkey, self.client.commitment())
            .await
            .with_context(|| {
                format!(
                    "failed to fetch domain record {pubkey} from {}",
                    self.client.url()
                )
            })?;
        response
            .value
            .map(|account| {
                ErRecord::deserialize(&mut account.data())
                    .context("failed to decode domain record")
            })
            .transpose()
    }

    async fn send<T: borsh::BorshSerialize>(
        &self,
        payer: &Keypair,
        pda: Pubkey,
        instruction: T,
    ) -> Result<()> {
        let accounts = vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(pda, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ];
        let instruction =
            Instruction::new_with_borsh(ID, &instruction, accounts);
        let blockhash = self
            .client
            .get_latest_blockhash()
            .await
            .context("failed to get latest blockhash")?;
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer.pubkey()),
            &[payer],
            blockhash,
        );
        self.client
            .send_and_confirm_transaction(&transaction)
            .await
            .context("failed to send and confirm domain transaction")?;
        Ok(())
    }
}

fn record_pda(identity: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[ER_RECORD_SEED, identity.as_ref()], &ID).0
}
