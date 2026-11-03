//! Signed domain-program operations whose outcomes use confirmed base-chain commitment.

use anyhow::{Context, Result};
use mdp::{
    ID,
    consts::ER_RECORD_SEED,
    instructions::{
        Instruction as DomainInstruction, sync::SyncInstruction, version::v0::SyncRecordV0,
    },
    state::record::ErRecord,
};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk_ids::system_program;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tracing::info;

/// Domain-program client with the record identity acting as transaction payer and signer.
pub(super) struct Client(RpcClient);

impl Client {
    /// Uses confirmed commitment for domain transaction outcomes.
    pub(super) fn new(url: &str) -> Self {
        Self(RpcClient::new_with_commitment(
            url.to_owned(),
            CommitmentConfig::confirmed(),
        ))
    }

    /// Creates the identity's record; the program rejects an already registered identity.
    pub(super) async fn register(&self, payer: &Keypair, record: ErRecord) -> Result<()> {
        self.send(payer, record.pda().0, DomainInstruction::Register(record))
            .await
            .context("failed to register domain record")
    }

    /// Submits a full V0 update without fetching or merging the current record.
    /// The pinned program applies the supplied fields except for country-code changes.
    pub(super) async fn sync(&self, payer: &Keypair, record: ErRecord) -> Result<()> {
        let pda = record.pda().0;
        let ErRecord::V0(record) = record;
        let update = SyncRecordV0 {
            identity: record.identity,
            status: Some(record.status),
            block_time_ms: Some(record.block_time_ms),
            base_fee: Some(record.base_fee),
            features: Some(record.features),
            load_average: Some(record.load_average),
            country_code: Some(record.country_code),
            addr: Some(record.addr),
        };
        self.send(
            payer,
            pda,
            DomainInstruction::Sync(SyncInstruction::V0(update)),
        )
        .await
        .context("failed to synchronize domain record")
    }

    /// Removes the identity's record, leaving existence and authority checks to the program.
    pub(super) async fn unregister(&self, payer: &Keypair) -> Result<()> {
        let identity = payer.pubkey();
        let pda = Pubkey::find_program_address(&[ER_RECORD_SEED, identity.as_ref()], &ID).0;
        self.send(payer, pda, DomainInstruction::Unregister(identity))
            .await
            .context("failed to unregister domain record")
    }

    /// Signs the domain instruction as its record identity and waits for base-chain confirmation.
    async fn send(
        &self,
        payer: &Keypair,
        pda: Pubkey,
        instruction: DomainInstruction,
    ) -> Result<()> {
        let accounts = vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(pda, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ];
        let instruction = Instruction::new_with_borsh(ID, &instruction, accounts);
        let blockhash =
            self.0.get_latest_blockhash().await.context("failed to get latest blockhash")?;
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer.pubkey()),
            &[payer],
            blockhash,
        );
        let signature = self
            .0
            .send_and_confirm_transaction(&transaction)
            .await
            .context("failed to send and confirm domain transaction")?;
        info!(%signature, "Domain transaction confirmed");
        Ok(())
    }
}
