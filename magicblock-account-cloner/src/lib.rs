//! Chainlink cloner - clones accounts from remote chain to ephemeral validator.
//!
//! # Account Cloning
//!
//! Accounts are cloned via direct encoding in transactions:
//! - Small accounts (<63KB): Single `CloneAccount` instruction
//! - Large accounts (>=63KB): `CloneAccountInit` → `CloneAccountContinue`* sequence
//!
//! # Program Cloning
//!
//! Programs use a buffer-based approach to handle loader-specific logic:
//!
//! ## V1 Programs (bpf_loader)
//! Converted to V3 (upgradeable loader) format:
//! 1. Clone ELF to buffer account
//! 2. `FinalizeV1ProgramFromBuffer` creates program + program_data accounts
//!
//! ## V4 Programs (loader_v4)
//! 1. Clone ELF to buffer account
//! 2. `FinalizeProgramFromBuffer` creates program account with LoaderV4 header
//! 3. `LoaderV4::Deploy` is called
//! 4. `SetProgramAuthority` sets the chain's authority
//!
//! # Buffer Account
//!
//! The buffer is a temporary account that holds the raw ELF data during cloning.
//! It's derived as a PDA: `["buffer", program_id]` owned by validator authority.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::{
    cloner::{
        errors::{ClonerError, ClonerResult},
        AccountCloneRequest, Cloner,
    },
    remote_account_provider::program_account::{
        LoadedProgram, RemoteProgramLoader,
    },
};
use magicblock_committor_service::{BaseIntentCommittor, CommittorService};
use magicblock_config::config::ChainLinkConfig;
use magicblock_core::link::transactions::TransactionSchedulerHandle;
use magicblock_ledger::LatestBlock;
use magicblock_magic_program_api::{
    args::ScheduleTaskArgs,
    instruction::{AccountCloneFields, MagicBlockInstruction},
    MAGIC_CONTEXT_PUBKEY,
};
use magicblock_program::{
    instruction_utils::InstructionUtils,
    validator::{validator_authority, validator_authority_id},
};
use solana_account::ReadableAccount;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_loader_v4_interface::{
    instruction::LoaderV4Instruction,
    state::LoaderV4Status,
};
use solana_pubkey::Pubkey;
use solana_sdk_ids::{bpf_loader_upgradeable, loader_v4};
use solana_signature::Signature;
use solana_signer::Signer;
use solana_sysvar::rent::Rent;
use solana_transaction::Transaction;
use tracing::*;

/// Max data that fits in a single transaction (~63KB)
pub const MAX_INLINE_DATA_SIZE: usize = 63 * 1024;

mod account_cloner;
mod util;

pub use account_cloner::*;
pub use util::derive_buffer_pubkey;

pub struct ChainlinkCloner {
    changeset_committor: Option<Arc<CommittorService>>,
    config: ChainLinkConfig,
    tx_scheduler: TransactionSchedulerHandle,
    accounts_db: Arc<AccountsDb>,
    block: LatestBlock,
}

impl ChainlinkCloner {
    pub fn new(
        changeset_committor: Option<Arc<CommittorService>>,
        config: ChainLinkConfig,
        tx_scheduler: TransactionSchedulerHandle,
        accounts_db: Arc<AccountsDb>,
        block: LatestBlock,
    ) -> Self {
        Self { changeset_committor, config, tx_scheduler, accounts_db, block }
    }

    // -----------------
    // Transaction Helpers
    // -----------------

    async fn send_tx(&self, tx: Transaction) -> ClonerResult<Signature> {
        let sig = tx.signatures[0];
        self.tx_scheduler.execute(tx).await?;
        Ok(sig)
    }

    fn sign_tx(&self, ixs: &[Instruction], blockhash: Hash) -> Transaction {
        let kp = validator_authority();
        Transaction::new_signed_with_payer(ixs, Some(&kp.pubkey()), &[&kp], blockhash)
    }

    // -----------------
    // Instruction Builders
    // -----------------

    fn clone_ix(pubkey: Pubkey, data: Vec<u8>, fields: AccountCloneFields) -> Instruction {
        Instruction::new_with_bincode(
            magicblock_program::ID,
            &MagicBlockInstruction::CloneAccount { pubkey, data, fields },
            clone_account_metas(pubkey),
        )
    }

    fn clone_init_ix(
        pubkey: Pubkey,
        total_len: u32,
        initial_data: Vec<u8>,
        fields: AccountCloneFields,
    ) -> Instruction {
        Instruction::new_with_bincode(
            magicblock_program::ID,
            &MagicBlockInstruction::CloneAccountInit {
                pubkey,
                total_data_len: total_len,
                initial_data,
                fields,
            },
            clone_account_metas(pubkey),
        )
    }

    fn clone_continue_ix(
        pubkey: Pubkey,
        offset: u32,
        data: Vec<u8>,
        is_last: bool,
    ) -> Instruction {
        Instruction::new_with_bincode(
            magicblock_program::ID,
            &MagicBlockInstruction::CloneAccountContinue { pubkey, offset, data, is_last },
            clone_account_metas(pubkey),
        )
    }

    fn cleanup_ix(pubkey: Pubkey) -> Instruction {
        Instruction::new_with_bincode(
            magicblock_program::ID,
            &MagicBlockInstruction::CleanupPartialClone { pubkey },
            clone_account_metas(pubkey),
        )
    }

    fn finalize_program_ix(program: Pubkey, buffer: Pubkey, slot: u64) -> Instruction {
        Instruction::new_with_bincode(
            magicblock_program::ID,
            &MagicBlockInstruction::FinalizeProgramFromBuffer { slot },
            vec![
                AccountMeta::new_readonly(validator_authority_id(), true),
                AccountMeta::new(program, false),
                AccountMeta::new(buffer, false),
            ],
        )
    }

    fn set_authority_ix(program: Pubkey, authority: Pubkey) -> Instruction {
        Instruction::new_with_bincode(
            magicblock_program::ID,
            &MagicBlockInstruction::SetProgramAuthority { authority },
            vec![
                AccountMeta::new_readonly(validator_authority_id(), true),
                AccountMeta::new(program, false),
            ],
        )
    }

    // -----------------
    // Clone Fields Helper
    // -----------------

    fn clone_fields(request: &AccountCloneRequest) -> AccountCloneFields {
        AccountCloneFields {
            lamports: request.account.lamports(),
            owner: *request.account.owner(),
            executable: request.account.executable(),
            delegated: request.account.delegated(),
            confined: request.account.confined(),
            remote_slot: request.account.remote_slot(),
        }
    }

    // -----------------
    // Account Cloning
    // -----------------

    fn build_small_account_tx(
        &self,
        request: &AccountCloneRequest,
        blockhash: Hash,
    ) -> Transaction {
        let fields = Self::clone_fields(request);
        let clone_ix = Self::clone_ix(request.pubkey, request.account.data().to_vec(), fields);

        // TODO(#625): Re-enable frequency commits when proper limits are in place:
        // 1. Allow configuring a higher minimum frequency
        // 2. Stop committing accounts if they have been committed more than X times
        //    where X corresponds to what we can charge
        //
        // To re-enable, uncomment the following and use `ixs` instead of `[clone_ix]`:
        // let ixs = self.maybe_add_crank_commits_ix(request, clone_ix);
        let ixs = vec![clone_ix];

        self.sign_tx(&ixs, blockhash)
    }

    /// Builds crank commits instruction for periodic account commits.
    /// Currently disabled - see https://github.com/magicblock-labs/magicblock-validator/issues/625
    #[allow(dead_code)]
    fn build_crank_commits_ix(pubkey: Pubkey, commit_frequency_ms: i64) -> Instruction {
        let task_id: i64 = rand::random();
        let schedule_commit_ix = Instruction::new_with_bincode(
            magicblock_program::ID,
            &MagicBlockInstruction::ScheduleCommit,
            vec![
                AccountMeta::new(validator_authority_id(), true),
                AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
                AccountMeta::new_readonly(pubkey, false),
            ],
        );
        InstructionUtils::schedule_task_instruction(
            &validator_authority_id(),
            ScheduleTaskArgs {
                task_id,
                execution_interval_millis: commit_frequency_ms,
                iterations: i64::MAX,
                instructions: vec![schedule_commit_ix],
            },
            &[pubkey, MAGIC_CONTEXT_PUBKEY, validator_authority_id()],
        )
    }

    fn build_large_account_txs(
        &self,
        request: &AccountCloneRequest,
        blockhash: Hash,
    ) -> Vec<Transaction> {
        let data = request.account.data();
        let fields = Self::clone_fields(request);
        let mut txs = Vec::new();

        // Init tx with first chunk
        let first_chunk = data[..MAX_INLINE_DATA_SIZE.min(data.len())].to_vec();
        let init_ix = Self::clone_init_ix(request.pubkey, data.len() as u32, first_chunk, fields);
        txs.push(self.sign_tx(&[init_ix], blockhash));

        // Continue txs for remaining chunks
        let mut offset = MAX_INLINE_DATA_SIZE;
        while offset < data.len() {
            let end = (offset + MAX_INLINE_DATA_SIZE).min(data.len());
            let chunk = data[offset..end].to_vec();
            let is_last = end == data.len();

            let continue_ix = Self::clone_continue_ix(request.pubkey, offset as u32, chunk, is_last);
            txs.push(self.sign_tx(&[continue_ix], blockhash));
            offset = end;
        }

        txs
    }

    async fn send_cleanup(&self, pubkey: Pubkey) {
        let blockhash = self.block.load().blockhash;
        let tx = self.sign_tx(&[Self::cleanup_ix(pubkey)], blockhash);
        if let Err(e) = self.send_tx(tx).await {
            error!(pubkey = %pubkey, error = ?e, "Failed to cleanup partial clone");
        }
    }

    // -----------------
    // Program Cloning
    // -----------------

    fn build_program_txs(
        &self,
        program: LoadedProgram,
        blockhash: Hash,
    ) -> ClonerResult<Option<Vec<Transaction>>> {
        match program.loader {
            RemoteProgramLoader::V1 => self.build_v1_program_txs(program, blockhash),
            _ => self.build_v4_program_txs(program, blockhash),
        }
    }

    /// V1 programs are converted to V3 (upgradeable loader) format.
    /// Supports programs of any size via multi-transaction cloning.
    fn build_v1_program_txs(
        &self,
        program: LoadedProgram,
        blockhash: Hash,
    ) -> ClonerResult<Option<Vec<Transaction>>> {
        let program_id = program.program_id;
        let chain_authority = program.authority;

        debug!(program_id = %program_id, "Loading V1 program as V3 format");

        let slot = self.accounts_db.slot().saturating_sub(5).max(1);
        let elf_data = program.program_data;
        let (buffer_pubkey, _) = derive_buffer_pubkey(&program_id);
        let (program_data_addr, _) = Pubkey::find_program_address(
            &[program_id.as_ref()],
            &bpf_loader_upgradeable::id(),
        );

        // Buffer is a dummy account owned by system program, just holds raw ELF data
        let lamports = Rent::default().minimum_balance(elf_data.len());
        let buffer_fields = AccountCloneFields {
            lamports,
            owner: solana_sdk_ids::system_program::id(),
            ..Default::default()
        };

        // Finalization instruction
        // Must wrap in disable/enable executable check since finalize sets executable=true
        let finalize_ixs = vec![
            InstructionUtils::disable_executable_check_instruction(&validator_authority_id()),
            Self::finalize_v1_program_ix(
                program_id,
                program_data_addr,
                buffer_pubkey,
                slot,
                chain_authority,
            ),
            InstructionUtils::enable_executable_check_instruction(&validator_authority_id()),
        ];

        // Build transactions based on ELF size
        let txs = if elf_data.len() <= MAX_INLINE_DATA_SIZE {
            // Small: single transaction with clone + finalize
            let ixs = vec![
                Self::clone_ix(buffer_pubkey, elf_data, buffer_fields),
            ]
            .into_iter()
            .chain(finalize_ixs)
            .collect::<Vec<_>>();
            vec![self.sign_tx(&ixs, blockhash)]
        } else {
            // Large: multi-transaction flow
            self.build_large_program_txs(
                buffer_pubkey,
                elf_data,
                buffer_fields,
                finalize_ixs,
                blockhash,
            )
        };

        Ok(Some(txs))
    }

    /// Builds finalize instruction for V1 programs (creates V3 accounts from buffer).
    fn finalize_v1_program_ix(
        program: Pubkey,
        program_data: Pubkey,
        buffer: Pubkey,
        slot: u64,
        authority: Pubkey,
    ) -> Instruction {
        Instruction::new_with_bincode(
            magicblock_program::ID,
            &MagicBlockInstruction::FinalizeV1ProgramFromBuffer { slot, authority },
            vec![
                AccountMeta::new_readonly(validator_authority_id(), true),
                AccountMeta::new(program, false),
                AccountMeta::new(program_data, false),
                AccountMeta::new(buffer, false),
            ],
        )
    }

    /// V2/V3/V4 programs use LoaderV4 with proper deploy flow.
    /// Supports programs of any size via multi-transaction cloning.
    fn build_v4_program_txs(
        &self,
        program: LoadedProgram,
        blockhash: Hash,
    ) -> ClonerResult<Option<Vec<Transaction>>> {
        let program_id = program.program_id;
        let chain_authority = program.authority;

        // Skip retracted programs
        if matches!(program.loader_status, LoaderV4Status::Retracted) {
            debug!(program_id = %program_id, "Program is retracted on chain");
            return Ok(None);
        }

        debug!(program_id = %program_id, "Deploying program with V4 loader");

        let slot = self.accounts_db.slot();
        let program_data = program.program_data;
        let (buffer_pubkey, _) = derive_buffer_pubkey(&program_id);

        // Buffer is a dummy account owned by system program, just holds raw ELF data
        let lamports = Rent::default().minimum_balance(program_data.len());
        let buffer_fields = AccountCloneFields {
            lamports,
            owner: solana_sdk_ids::system_program::id(),
            ..Default::default()
        };

        let deploy_ix = Instruction {
            program_id: loader_v4::id(),
            accounts: vec![
                AccountMeta::new(program_id, false),
                AccountMeta::new_readonly(validator_authority_id(), true),
            ],
            data: bincode::serialize(&LoaderV4Instruction::Deploy)
                .map_err(|e| ClonerError::SerializationError(e.to_string()))?,
        };

        // Finalization instructions (always in last tx)
        // Must wrap in disable/enable executable check since finalize sets executable=true
        let finalize_ixs = vec![
            InstructionUtils::disable_executable_check_instruction(&validator_authority_id()),
            Self::finalize_program_ix(program_id, buffer_pubkey, slot),
            deploy_ix,
            Self::set_authority_ix(program_id, chain_authority),
            InstructionUtils::enable_executable_check_instruction(&validator_authority_id()),
        ];

        // Build transactions based on program_data size
        let txs = if program_data.len() <= MAX_INLINE_DATA_SIZE {
            // Small: single transaction
            let ixs = vec![
                Self::clone_ix(buffer_pubkey, program_data, buffer_fields),
            ]
            .into_iter()
            .chain(finalize_ixs)
            .collect::<Vec<_>>();
            vec![self.sign_tx(&ixs, blockhash)]
        } else {
            // Large: multi-transaction flow
            self.build_large_program_txs(
                buffer_pubkey,
                program_data,
                buffer_fields,
                finalize_ixs,
                blockhash,
            )
        };

        Ok(Some(txs))
    }

    /// Builds multi-transaction flow for large programs (any loader).
    fn build_large_program_txs(
        &self,
        buffer_pubkey: Pubkey,
        program_data: Vec<u8>,
        fields: AccountCloneFields,
        finalize_ixs: Vec<Instruction>,
        blockhash: Hash,
    ) -> Vec<Transaction> {
        let mut txs = Vec::new();
        let total_len = program_data.len() as u32;
        let num_chunks = (total_len as usize).div_ceil(MAX_INLINE_DATA_SIZE);

        info!(
            buffer = %buffer_pubkey,
            total_len,
            num_chunks,
            "Building large program clone transactions"
        );

        // First chunk via Init
        let first_chunk = program_data[..MAX_INLINE_DATA_SIZE.min(program_data.len())].to_vec();
        let init_ix = Self::clone_init_ix(buffer_pubkey, total_len, first_chunk, fields);
        txs.push(self.sign_tx(&[init_ix], blockhash));

        // Continue chunks
        let mut offset = MAX_INLINE_DATA_SIZE;
        let mut chunk_num = 1;
        while offset < program_data.len() {
            let end = (offset + MAX_INLINE_DATA_SIZE).min(program_data.len());
            let chunk = program_data[offset..end].to_vec();
            let is_last = end == program_data.len();
            chunk_num += 1;

            if is_last {
                // Last chunk + finalize instructions in single tx
                info!(
                    buffer = %buffer_pubkey,
                    chunk = chunk_num,
                    num_chunks,
                    finalize_ixs_count = finalize_ixs.len(),
                    "Building final transaction with continue + finalize"
                );
                let continue_ix = Self::clone_continue_ix(buffer_pubkey, offset as u32, chunk, true);
                let ixs = vec![continue_ix]
                    .into_iter()
                    .chain(finalize_ixs.clone())
                    .collect::<Vec<_>>();
                txs.push(self.sign_tx(&ixs, blockhash));
            } else {
                let continue_ix = Self::clone_continue_ix(buffer_pubkey, offset as u32, chunk, false);
                txs.push(self.sign_tx(&[continue_ix], blockhash));
            }
            offset = end;
        }

        txs
    }

    // -----------------
    // Lookup Tables
    // -----------------

    fn maybe_prepare_lookup_tables(&self, pubkey: Pubkey, owner: Pubkey) {
        if let Some(committor) = self.changeset_committor.as_ref() {
            if self.config.prepare_lookup_tables {
                let committor = committor.clone();
                tokio::spawn(async move {
                    if let Err(e) = committor.reserve_pubkeys_for_committee(pubkey, owner).await {
                        error!(error = ?e, "Failed to reserve lookup tables");
                    }
                });
            }
        }
    }
}

/// Shared account metas for clone instructions.
fn clone_account_metas(pubkey: Pubkey) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(validator_authority_id(), true),
        AccountMeta::new(pubkey, false),
    ]
}

#[async_trait]
impl Cloner for ChainlinkCloner {
    async fn clone_account(
        &self,
        request: AccountCloneRequest,
    ) -> ClonerResult<Signature> {
        let blockhash = self.block.load().blockhash;
        let data_len = request.account.data().len();

        if request.account.delegated() {
            self.maybe_prepare_lookup_tables(request.pubkey, *request.account.owner());
        }

        // Small account: single tx
        if data_len <= MAX_INLINE_DATA_SIZE {
            let tx = self.build_small_account_tx(&request, blockhash);
            return self.send_tx(tx).await.map_err(|e| {
                ClonerError::FailedToCloneRegularAccount(request.pubkey, Box::new(e))
            });
        }

        // Large account: multi-tx with cleanup on failure
        let txs = self.build_large_account_txs(&request, blockhash);

        let mut last_sig = Signature::default();
        for tx in txs {
            match self.send_tx(tx).await {
                Ok(sig) => last_sig = sig,
                Err(e) => {
                    self.send_cleanup(request.pubkey).await;
                    return Err(ClonerError::FailedToCloneRegularAccount(
                        request.pubkey,
                        Box::new(e),
                    ));
                }
            }
        }

        Ok(last_sig)
    }

    async fn clone_program(
        &self,
        program: LoadedProgram,
    ) -> ClonerResult<Signature> {
        let blockhash = self.block.load().blockhash;
        let program_id = program.program_id;

        let Some(txs) = self.build_program_txs(program, blockhash).map_err(|e| {
            ClonerError::FailedToCreateCloneProgramTransaction(program_id, Box::new(e))
        })?
        else {
            // Program was retracted
            return Ok(Signature::default());
        };

        // Both V1 and V4 use buffer_pubkey for multi-tx cloning
        let buffer_pubkey = derive_buffer_pubkey(&program_id).0;

        let mut last_sig = Signature::default();
        for tx in txs {
            match self.send_tx(tx).await {
                Ok(sig) => last_sig = sig,
                Err(e) => {
                    self.send_cleanup(buffer_pubkey).await;
                    return Err(ClonerError::FailedToCloneProgram(
                        program_id,
                        Box::new(e),
                    ));
                }
            }
        }

        // Wait one slot for program to become usable
        let current_slot = self.accounts_db.slot();
        while self.accounts_db.slot() == current_slot {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        Ok(last_sig)
    }
}
