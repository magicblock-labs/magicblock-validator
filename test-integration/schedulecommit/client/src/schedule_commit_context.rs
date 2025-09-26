use log::*;
use std::{fmt, ops::Deref};

use anyhow::{Context, Result};
use integration_test_tools::IntegrationTestContext;
use program_schedulecommit::api::{
    delegate_account_cpi_instruction, init_account_instruction,
    init_payer_escrow, pda_and_bump,
};
use solana_rpc_client::rpc_client::{RpcClient, SerializableTransaction};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
#[allow(unused_imports)]
use solana_sdk::signer::SeedDerivable;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

pub struct ScheduleCommitTestContext {
    // The first payer from the committees array which is used to fund transactions inside the
    // ephemeral
    pub payer_ephem: Keypair,
    // The first payer from the committees array which is used to fund transactions on chain
    pub payer_chain: Keypair,
    // The Payer keypairs along with its PDA pubkey which we'll commit
    pub committees: Vec<(Keypair, Pubkey)>,

    common_ctx: IntegrationTestContext,
}

impl fmt::Display for ScheduleCommitTestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ScheduleCommitTestContext {{ committees: [")?;
        for (payer, pda) in &self.committees {
            writeln!(f, "Payer: {} PDA: {}, ", payer.pubkey(), pda)?;
        }
        writeln!(f, "] }}")
    }
}

pub struct ScheduleCommitTestContextFields<'a> {
    pub payer_ephem: &'a Keypair,
    pub payer_chain: &'a Keypair,
    pub committees: &'a Vec<(Keypair, Pubkey)>,
    pub commitment: &'a CommitmentConfig,
    pub chain_client: Option<&'a RpcClient>,
    pub ephem_client: &'a RpcClient,
    pub validator_identity: &'a Pubkey,
    pub chain_blockhash: Option<&'a Hash>,
    pub ephem_blockhash: &'a Hash,
}

impl ScheduleCommitTestContext {
    // -----------------
    // Init
    // -----------------
    pub fn try_new_random_keys(ncommittees: usize) -> Result<Self> {
        Self::try_new_internal(ncommittees, true)
    }
    pub fn try_new(ncommittees: usize) -> Result<Self> {
        Self::try_new_internal(ncommittees, false)
    }

    fn try_new_internal(ncommittees: usize, random_keys: bool) -> Result<Self> {
        let ictx = IntegrationTestContext::try_new()?;

        let payer_chain = if random_keys {
            Keypair::new()
        } else {
            Keypair::from_seed(&[0u8; 32]).unwrap()
        };
        let lamports = LAMPORTS_PER_SOL * 10;
        let payer_chain_airdrop_sig =
            ictx.airdrop_chain(&payer_chain.pubkey(), lamports)?;
        debug!(
            "Airdropped {} lamports to chain payer {} ({})",
            lamports,
            payer_chain.pubkey(),
            payer_chain_airdrop_sig
        );

        // Each committee is the payer and the matching PDA
        // The payer has money airdropped in order to init its PDA.
        // However in order to commit we can use any payer as the only
        // requirement is that the PDA is owned by its program.
        let committees = (0..ncommittees)
            .map(|_idx| {
                let payer_ephem = if random_keys {
                    Keypair::new()
                } else {
                    Keypair::from_seed(&[_idx as u8 + 100; 32]).unwrap()
                };
                ictx.airdrop_chain_and_delegate(
                    &payer_chain,
                    &payer_ephem,
                    lamports,
                )
                .unwrap();
                let (pda, _) = pda_and_bump(&payer_ephem.pubkey());
                (payer_ephem, pda)
            })
            .collect::<Vec<(Keypair, Pubkey)>>();

        let payer_ephem = committees[0].0.insecure_clone();
        Ok(Self {
            payer_chain,
            payer_ephem,
            committees,
            common_ctx: ictx,
        })
    }

    // -----------------
    // Schedule Commit specific Transactions
    // -----------------
    pub fn init_committees(&self) -> Result<Signature> {
        let ixs = self
            .committees
            .iter()
            .map(|(player, committee)| {
                init_account_instruction(
                    self.payer_chain.pubkey(),
                    player.pubkey(),
                    *committee,
                )
            })
            .collect::<Vec<_>>();

        let mut signers = self
            .committees
            .iter()
            .map(|(payer, _)| payer)
            .collect::<Vec<_>>();
        signers.push(&self.payer_chain);

        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&self.payer_chain.pubkey()),
            &signers,
            *self.try_chain_blockhash()?,
        );
        let sig = self.try_chain_client()?
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                self.commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .with_context(|| {
                format!(
                    "Failed to initialize committees. Transaction signature: {}",
                    tx.get_signature()
                )
            })?;

        debug!("Initialed committees: {sig}");
        Ok(sig)
    }

    pub fn escrow_lamports_for_payer(&self) -> Result<Signature> {
        let ixs = init_payer_escrow(self.payer_ephem.pubkey());

        // The init tx for all payers is funded by the first payer for simplicity
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&self.payer_ephem.pubkey()),
            &[&self.payer_ephem],
            *self.try_chain_blockhash()?,
        );
        self.try_chain_client()?
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                self.commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .with_context(|| "Failed to escrow fund for payer")
    }

    pub fn delegate_committees(
        &self,
        blockhash: Option<Hash>,
    ) -> Result<Signature> {
        let mut ixs = vec![];
        let mut payers = vec![];
        for (payer, _) in &self.committees {
            let ix = delegate_account_cpi_instruction(payer.pubkey());
            ixs.push(ix);
            payers.push(payer);
        }

        let blockhash = match blockhash {
            Some(blockhash) => blockhash,
            None => *self.try_chain_blockhash()?,
        };

        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&payers[0].pubkey()),
            &payers,
            blockhash,
        );
        self.try_chain_client()?
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                self.commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .with_context(|| {
                format!(
                    "Failed to delegate committees '{:?}'",
                    tx.signatures[0]
                )
            })
    }

    // -----------------
    // Integration Test Context Fields
    // -----------------
    pub fn try_chain_client(&self) -> anyhow::Result<&RpcClient> {
        let Some(chain_client) = self.chain_client.as_ref() else {
            return Err(anyhow::anyhow!("Chain client not available"));
        };
        Ok(chain_client)
    }

    pub fn try_chain_blockhash(&self) -> anyhow::Result<&Hash> {
        let Some(chain_blockhash) = self.chain_blockhash.as_ref() else {
            return Err(anyhow::anyhow!("Chain blockhash  not available"));
        };
        Ok(chain_blockhash)
    }

    pub fn ephem_client(&self) -> &RpcClient {
        self.common_ctx.try_ephem_client().unwrap()
    }
    pub fn ephem_blockhash(&self) -> &Hash {
        self.common_ctx.ephem_blockhash.as_ref().unwrap()
    }

    pub fn fields(&self) -> ScheduleCommitTestContextFields {
        ScheduleCommitTestContextFields {
            payer_chain: &self.payer_chain,
            payer_ephem: &self.payer_ephem,
            committees: &self.committees,
            commitment: &self.commitment,
            chain_client: self.common_ctx.chain_client.as_ref(),
            ephem_client: self.common_ctx.try_ephem_client().unwrap(),
            validator_identity: self
                .common_ctx
                .ephem_validator_identity
                .as_ref()
                .unwrap(),
            chain_blockhash: self.common_ctx.chain_blockhash.as_ref(),
            ephem_blockhash: self.common_ctx.ephem_blockhash.as_ref().unwrap(),
        }
    }
}

// -----------------
// Integration Test Methods and Fields exposed via Deref
// -----------------
impl Deref for ScheduleCommitTestContext {
    type Target = IntegrationTestContext;

    fn deref(&self) -> &Self::Target {
        &self.common_ctx
    }
}
