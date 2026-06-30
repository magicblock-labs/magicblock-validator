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

use async_trait::async_trait;
use magicblock_chainlink::{
    cloner::{
        errors::{ClonerError, ClonerResult},
        AccountCloneRequest, Cloner,
    },
    remote_account_provider::program_account::{
        LoadedProgram, RemoteProgramLoader,
    },
};
use magicblock_core::link::transactions::{
    with_encoded, TransactionSchedulerHandle,
};
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
    instruction::LoaderV4Instruction, state::LoaderV4Status,
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
const MAX_INLINE_TRANSACTION_SIZE: usize = u16::MAX as usize;

mod util;

pub use util::derive_buffer_pubkey;

pub struct ChainlinkCloner {
    tx_scheduler: TransactionSchedulerHandle,
    block: LatestBlock,
}

impl ChainlinkCloner {
    pub fn new(
        tx_scheduler: TransactionSchedulerHandle,
        block: LatestBlock,
    ) -> Self {
        Self {
            tx_scheduler,
            block,
        }
    }

    // -----------------
    // Transaction Helpers
    // -----------------

    async fn send_tx(&self, tx: Transaction) -> ClonerResult<Signature> {
        let sig = tx.signatures[0];
        self.tx_scheduler.execute(with_encoded(tx)?).await?;
        Ok(sig)
    }

    // -----------------
    fn create_signed_tx(
        &self,
        ixs: &[Instruction],
        blockhash: Hash,
    ) -> Transaction {
        let kp = validator_authority();
        Transaction::new_signed_with_payer(
            ixs,
            Some(&kp.pubkey()),
            &[&kp],
            blockhash,
        )
    }

    fn transaction_size(tx: &Transaction) -> ClonerResult<usize> {
        Ok(bincode::serialized_size(tx)?.try_into()?)
    }

    fn ensure_transaction_fits(
        pubkey: Pubkey,
        tx: &Transaction,
    ) -> ClonerResult<usize> {
        let tx_size = Self::transaction_size(tx)?;
        if tx_size > MAX_INLINE_TRANSACTION_SIZE {
            return Err(ClonerError::CloneTransactionTooLarge {
                pubkey,
                size: tx_size,
                max_size: MAX_INLINE_TRANSACTION_SIZE,
            });
        }
        Ok(tx_size)
    }

    fn ensure_transactions_fit(
        pubkey: Pubkey,
        txs: &[Transaction],
    ) -> ClonerResult<()> {
        for tx in txs {
            Self::ensure_transaction_fits(pubkey, tx)?;
        }
        Ok(())
    }

    // -----------------
    // Instruction Builders (delegates to InstructionUtils)
    // -----------------

    fn clone_ix(
        pubkey: Pubkey,
        data: Vec<u8>,
        fields: AccountCloneFields,
        actions: Vec<Instruction>,
    ) -> Instruction {
        InstructionUtils::clone_account_instruction(
            pubkey, data, fields, actions,
        )
    }

    fn clone_init_ix(
        pubkey: Pubkey,
        total_len: u32,
        initial_data: Vec<u8>,
        fields: AccountCloneFields,
    ) -> Instruction {
        InstructionUtils::clone_account_init_instruction(
            pubkey,
            total_len,
            initial_data,
            fields,
        )
    }

    fn clone_continue_ix(
        pubkey: Pubkey,
        offset: u32,
        data: Vec<u8>,
        is_last: bool,
        actions: Vec<Instruction>,
        needs_undelegation: bool,
    ) -> Instruction {
        InstructionUtils::clone_account_continue_instruction(
            pubkey,
            offset,
            data,
            is_last,
            actions,
            needs_undelegation,
        )
    }

    fn cleanup_ix(pubkey: Pubkey) -> Instruction {
        InstructionUtils::cleanup_partial_clone_instruction(pubkey)
    }

    fn post_delegation_action_ix(
        delegated_account_pubkey: Pubkey,
        actions: Vec<Instruction>,
    ) -> Instruction {
        InstructionUtils::post_delegation_action_executor_instruction(
            delegated_account_pubkey,
            actions,
        )
    }

    fn finalize_program_ix(
        program: Pubkey,
        buffer: Pubkey,
        remote_slot: u64,
    ) -> Instruction {
        InstructionUtils::finalize_program_from_buffer_instruction(
            program,
            buffer,
            remote_slot,
        )
    }

    fn set_authority_ix(program: Pubkey, authority: Pubkey) -> Instruction {
        InstructionUtils::set_program_authority_instruction(program, authority)
    }

    fn schedule_undelegation_ix(pubkey: Pubkey) -> Instruction {
        InstructionUtils::schedule_cloned_account_undelegation_instruction(
            pubkey,
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

    // Account Cloning
    // -----------------

    fn build_small_account_tx(
        &self,
        request: &AccountCloneRequest,
        blockhash: Hash,
    ) -> Transaction {
        let fields = Self::clone_fields(request);
        let actions: Vec<Instruction> =
            request.delegation_actions.clone().into();
        let clone_actions = if request.needs_undelegation {
            Vec::new()
        } else {
            actions.clone()
        };
        let clone_ix = Self::clone_ix(
            request.pubkey,
            request.account.data().to_vec(),
            fields,
            clone_actions,
        );

        // TODO(#625): Re-enable frequency commits when proper limits are in place:
        // 1. Allow configuring a higher minimum frequency
        // 2. Stop committing accounts if they have been committed more than X times
        //    where X corresponds to what we can charge
        //
        // To re-enable, uncomment the following and use `ixs` instead of `[clone_ix]`:
        // let ixs = self.maybe_add_crank_commits_ix(request, clone_ix);
        let mut ixs = vec![clone_ix];
        if request.needs_undelegation {
            ixs.push(Self::schedule_undelegation_ix(request.pubkey));
        } else if !actions.is_empty() {
            ixs.push(Self::post_delegation_action_ix(request.pubkey, actions));
        }

        self.create_signed_tx(&ixs, blockhash)
    }

    /// Builds crank commits instruction for periodic account commits.
    /// Currently disabled - see https://github.com/magicblock-labs/magicblock-validator/issues/625
    #[allow(dead_code)]
    fn build_crank_commits_ix(
        pubkey: Pubkey,
        commit_frequency_ms: i64,
    ) -> Instruction {
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
        let init_ix = Self::clone_init_ix(
            request.pubkey,
            // we assume the cloned accounts do not have data field
            // not exceeding the max solana limit, which is always true
            // since the source of cloning is always base chain
            data.len() as u32,
            first_chunk,
            fields,
        );
        txs.push(self.create_signed_tx(&[init_ix], blockhash));

        let actions: Vec<Instruction> =
            request.delegation_actions.clone().into();
        let clone_actions = if request.needs_undelegation {
            Vec::new()
        } else {
            actions.clone()
        };

        // Continue txs for remaining chunks
        let has_post_delegation_action =
            !clone_actions.is_empty() || request.needs_undelegation;
        let mut offset = MAX_INLINE_DATA_SIZE;
        while offset < data.len() {
            let end = (offset + MAX_INLINE_DATA_SIZE).min(data.len());
            let chunk = data[offset..end].to_vec();
            let is_last = end == data.len();
            let final_without_actions = is_last && !has_post_delegation_action;

            let continue_ix = Self::clone_continue_ix(
                request.pubkey,
                offset as u32,
                chunk,
                final_without_actions,
                Vec::new(),
                false,
            );
            txs.push(self.create_signed_tx(&[continue_ix], blockhash));
            offset = end;
        }

        if request.needs_undelegation || !actions.is_empty() {
            let continue_ix = Self::clone_continue_ix(
                request.pubkey,
                data.len() as u32,
                Vec::new(),
                true,
                clone_actions,
                request.needs_undelegation,
            );
            let ix = if request.needs_undelegation {
                Self::schedule_undelegation_ix(request.pubkey)
            } else {
                Self::post_delegation_action_ix(request.pubkey, actions)
            };
            txs.push(self.create_signed_tx(&[continue_ix, ix], blockhash));
        }

        txs
    }

    async fn send_cleanup(&self, pubkey: Pubkey) {
        let blockhash = self.block.load().blockhash;
        let tx = self.create_signed_tx(&[Self::cleanup_ix(pubkey)], blockhash);
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
            RemoteProgramLoader::V1 => {
                self.build_v1_program_txs(program, blockhash)
            }
            _ => self.build_v4_program_txs(program, blockhash),
        }
    }

    /// Helper to build buffer fields for program cloning.
    fn buffer_fields(data_len: usize) -> AccountCloneFields {
        let lamports = Rent::default().minimum_balance(data_len);
        AccountCloneFields {
            lamports,
            owner: solana_sdk_ids::system_program::id(),
            ..Default::default()
        }
    }

    /// Helper to build program transactions (shared between V1 and V4).
    fn build_program_txs_from_finalize(
        &self,
        buffer_pubkey: Pubkey,
        program_data: Vec<u8>,
        finalize_ixs: Vec<Instruction>,
        blockhash: Hash,
    ) -> Vec<Transaction> {
        let buffer_fields = Self::buffer_fields(program_data.len());

        if program_data.len() <= MAX_INLINE_DATA_SIZE {
            // Small: single transaction with clone + finalize
            let mut ixs = vec![Self::clone_ix(
                buffer_pubkey,
                program_data,
                buffer_fields,
                Vec::new(),
            )];
            ixs.extend(finalize_ixs);
            vec![self.create_signed_tx(&ixs, blockhash)]
        } else {
            // Large: multi-transaction flow
            self.build_large_program_txs(
                buffer_pubkey,
                program_data,
                buffer_fields,
                finalize_ixs,
                blockhash,
            )
        }
    }

    /// V1 programs are converted to V3 (upgradeable loader) format.
    /// Supports programs of any size via multi-transaction cloning.
    ///
    /// NOTE: we don't support modifying this kind of program once it was
    /// deployed into our validator once.
    /// By nature of being immutable on chain this should never happen.
    /// Thus we avoid having to run the upgrade instruction and get
    /// away with just directly modifying the program and program data accounts.
    fn build_v1_program_txs(
        &self,
        program: LoadedProgram,
        blockhash: Hash,
    ) -> ClonerResult<Option<Vec<Transaction>>> {
        let program_id = program.program_id;
        let chain_authority = program.authority;
        let remote_slot = program.remote_slot;

        debug!(program_id = %program_id, "Loading V1 program as V3 format");

        let elf_data = program.program_data;
        let (buffer_pubkey, _) = derive_buffer_pubkey(&program_id);
        let (program_data_addr, _) = Pubkey::find_program_address(
            &[program_id.as_ref()],
            &bpf_loader_upgradeable::id(),
        );

        // Finalization instruction
        // Must wrap in disable/enable executable check since finalize sets executable=true
        let finalize_ixs = vec![
            InstructionUtils::disable_executable_check_instruction(
                &validator_authority_id(),
            ),
            InstructionUtils::finalize_v1_program_from_buffer_instruction(
                program_id,
                program_data_addr,
                buffer_pubkey,
                remote_slot,
                chain_authority,
            ),
            InstructionUtils::enable_executable_check_instruction(
                &validator_authority_id(),
            ),
        ];

        let txs = self.build_program_txs_from_finalize(
            buffer_pubkey,
            elf_data,
            finalize_ixs,
            blockhash,
        );

        Ok(Some(txs))
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
        let remote_slot = program.remote_slot;

        // Skip retracted programs
        if matches!(program.loader_status, LoaderV4Status::Retracted) {
            debug!(program_id = %program_id, "Program is retracted on chain");
            return Ok(None);
        }

        debug!(program_id = %program_id, "Deploying program with V4 loader");

        let program_data = program.program_data;
        let (buffer_pubkey, _) = derive_buffer_pubkey(&program_id);

        let deploy_ix = Instruction {
            program_id: loader_v4::id(),
            accounts: vec![
                AccountMeta::new(program_id, false),
                AccountMeta::new_readonly(validator_authority_id(), true),
            ],
            data: bincode::serialize(&LoaderV4Instruction::Deploy)?,
        };

        // Finalization instructions (always in last tx)
        // Must wrap in disable/enable executable check since finalize sets executable=true
        let finalize_ixs = vec![
            InstructionUtils::disable_executable_check_instruction(
                &validator_authority_id(),
            ),
            Self::finalize_program_ix(program_id, buffer_pubkey, remote_slot),
            deploy_ix,
            Self::set_authority_ix(program_id, chain_authority),
            InstructionUtils::enable_executable_check_instruction(
                &validator_authority_id(),
            ),
        ];

        let txs = self.build_program_txs_from_finalize(
            buffer_pubkey,
            program_data,
            finalize_ixs,
            blockhash,
        );

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
        let total_len = program_data.len() as u32;
        let num_chunks =
            total_len.div_ceil(MAX_INLINE_DATA_SIZE as u32) as usize;

        // First chunk via Init
        let first_chunk =
            &program_data[..MAX_INLINE_DATA_SIZE.min(program_data.len())];
        let init_ix = Self::clone_init_ix(
            buffer_pubkey,
            total_len,
            first_chunk.to_vec(),
            fields,
        );
        let mut txs = vec![self.create_signed_tx(&[init_ix], blockhash)];

        // Middle chunks (all except last)
        let last_offset = (num_chunks - 1) * MAX_INLINE_DATA_SIZE;
        for offset in
            (MAX_INLINE_DATA_SIZE..last_offset).step_by(MAX_INLINE_DATA_SIZE)
        {
            let chunk = &program_data[offset..offset + MAX_INLINE_DATA_SIZE];
            let continue_ix = Self::clone_continue_ix(
                buffer_pubkey,
                offset as u32,
                chunk.to_vec(),
                false,
                Vec::new(),
                false,
            );
            txs.push(self.create_signed_tx(&[continue_ix], blockhash));
        }

        // Last chunk with finalize instructions
        let last_chunk = &program_data[last_offset..];
        let mut ixs = vec![Self::clone_continue_ix(
            buffer_pubkey,
            last_offset as u32,
            last_chunk.to_vec(),
            true,
            Vec::new(),
            false,
        )];
        ixs.extend(finalize_ixs);
        txs.push(self.create_signed_tx(&ixs, blockhash));

        txs
    }
}

/// Shared account metas for clone instructions.
#[async_trait]
impl Cloner for ChainlinkCloner {
    async fn evict_account(&self, pubkey: Pubkey) -> ClonerResult<()> {
        let blockhash = self.block.load().blockhash;
        let evict_ix = InstructionUtils::evict_account_instruction(pubkey);
        let tx = self.create_signed_tx(&[evict_ix], blockhash);
        self.send_tx(tx).await.map_err(|err| {
            ClonerError::FailedToEvictAccount(pubkey, Box::new(err))
        })?;
        Ok(())
    }

    async fn clone_account(
        &self,
        request: AccountCloneRequest,
    ) -> ClonerResult<Signature> {
        let blockhash = self.block.load().blockhash;
        let data_len = request.account.data().len();

        if data_len <= MAX_INLINE_DATA_SIZE {
            let tx = self.build_small_account_tx(&request, blockhash);
            let tx_size = Self::transaction_size(&tx)?;
            if tx_size > MAX_INLINE_TRANSACTION_SIZE
                && !request.delegation_actions.is_empty()
            {
                debug!(
                    pubkey = %request.pubkey,
                    data_len,
                    tx_size,
                    "Small account clone transaction is too large, using chunked clone"
                );
            } else {
                let signature = self.send_tx(tx).await.map_err(|err| {
                    ClonerError::FailedToCloneRegularAccount(
                        request.pubkey,
                        Box::new(err),
                    )
                })?;

                return Ok(signature);
            }
        }

        // Large account: multi-tx with cleanup on failure
        let txs = self.build_large_account_txs(&request, blockhash);
        Self::ensure_transactions_fit(request.pubkey, &txs)?;

        let mut last_sig = None;
        for tx in txs {
            match self.send_tx(tx).await {
                Ok(sig) => {
                    last_sig.replace(sig);
                }
                Err(e) => {
                    self.send_cleanup(request.pubkey).await;
                    return Err(ClonerError::FailedToCloneRegularAccount(
                        request.pubkey,
                        Box::new(e),
                    ));
                }
            }
        }

        Ok(last_sig.unwrap_or_default())
    }

    async fn clone_program(
        &self,
        program: LoadedProgram,
    ) -> ClonerResult<Signature> {
        let blockhash = self.block.load().blockhash;
        let program_id = program.program_id;

        let Some(txs) =
            self.build_program_txs(program, blockhash).map_err(|e| {
                ClonerError::FailedToCreateCloneProgramTransaction(
                    program_id,
                    Box::new(e),
                )
            })?
        else {
            // Program was retracted
            return Ok(Signature::default());
        };

        // Both V1 and V4 use buffer_pubkey for multi-tx cloning
        let buffer_pubkey = derive_buffer_pubkey(&program_id).0;

        let mut last_sig = None;
        for tx in txs {
            match self.send_tx(tx).await {
                Ok(sig) => {
                    last_sig.replace(sig);
                }
                Err(e) => {
                    self.send_cleanup(buffer_pubkey).await;
                    return Err(ClonerError::FailedToCloneProgram(
                        program_id,
                        Box::new(e),
                    ));
                }
            }
        }

        // After cloning a program we need to wait at least one slot for it to become
        // usable, so we do that here
        let current_slot = self.block.load().slot;
        let mut block_updated = self.block.subscribe();
        while self.block.load().slot == current_slot {
            let _ = block_updated.recv().await;
        }

        Ok(last_sig.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use magicblock_chainlink::cloner::DelegationActions;
    use magicblock_core::link::link;
    use magicblock_magic_program_api::{
        instruction::{
            MagicBlockInstruction, PostDelegationActionExecutorInstruction,
        },
        POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID,
    };
    use solana_account::AccountSharedData;
    use solana_sdk_ids::system_program;

    use super::*;

    fn cloner() -> ChainlinkCloner {
        magicblock_program::validator::generate_validator_authority_if_needed();
        let (dispatch, _) = link();
        ChainlinkCloner::new(
            dispatch.transaction_scheduler,
            LatestBlock::default(),
        )
    }

    fn request(
        pubkey: Pubkey,
        data: Vec<u8>,
        actions: Vec<Instruction>,
    ) -> AccountCloneRequest {
        let mut account =
            AccountSharedData::new(1_000, data.len(), &system_program::id());
        account.set_data_from_slice(&data);
        account.set_delegated(true);
        AccountCloneRequest {
            pubkey,
            account,
            commit_frequency_ms: None,
            delegation_actions: DelegationActions::from(actions),
            delegated_to_other: None,
            needs_undelegation: false,
        }
    }

    fn action() -> Instruction {
        Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![AccountMeta::new(Pubkey::new_unique(), true)],
            data: vec![1, 2, 3],
        }
    }

    fn action_with_data_len(data_len: usize) -> Instruction {
        Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![AccountMeta::new(Pubkey::new_unique(), true)],
            data: vec![1; data_len],
        }
    }

    fn instruction_program_id(tx: &Transaction, ix_idx: usize) -> Pubkey {
        let ix = &tx.message().instructions[ix_idx];
        tx.message().account_keys[ix.program_id_index as usize]
    }

    fn instruction_data(tx: &Transaction, ix_idx: usize) -> &[u8] {
        &tx.message().instructions[ix_idx].data
    }

    #[test]
    fn transaction_size_matches_bincode_serialized_len() {
        let pubkey = Pubkey::new_unique();
        let tx = cloner().build_small_account_tx(
            &request(pubkey, vec![1, 2, 3], vec![action()]),
            Hash::default(),
        );

        assert_eq!(
            ChainlinkCloner::transaction_size(&tx).unwrap(),
            bincode::serialize(&tx).unwrap().len()
        );
    }

    #[test]
    fn oversized_chunked_action_transaction_fails_preflight() {
        let pubkey = Pubkey::new_unique();
        let actions =
            vec![action_with_data_len(MAX_INLINE_TRANSACTION_SIZE / 2 + 1024)];
        let txs = cloner().build_large_account_txs(
            &request(pubkey, vec![1, 2, 3], actions),
            Hash::default(),
        );
        let final_tx_size =
            ChainlinkCloner::transaction_size(txs.last().unwrap()).unwrap();

        assert!(final_tx_size > MAX_INLINE_TRANSACTION_SIZE);

        let err =
            ChainlinkCloner::ensure_transactions_fit(pubkey, &txs).unwrap_err();
        match err {
            ClonerError::CloneTransactionTooLarge {
                pubkey: err_pubkey,
                size,
                max_size,
            } => {
                assert_eq!(err_pubkey, pubkey);
                assert_eq!(size, final_tx_size);
                assert_eq!(max_size, MAX_INLINE_TRANSACTION_SIZE);
            }
            err => panic!("unexpected error: {err:?}"),
        }
    }

    #[test]
    fn undelegation_ix_uses_specific_magic_instruction() {
        magicblock_program::validator::generate_validator_authority_if_needed();
        let pubkey = Pubkey::new_unique();
        let ix = ChainlinkCloner::schedule_undelegation_ix(pubkey);

        assert_eq!(ix.program_id, POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID);
        assert_eq!(ix.accounts.len(), 4);
        assert_eq!(
            ix.accounts[0].pubkey,
            magicblock_program::validator::validator_authority_id()
        );
        assert!(ix.accounts[0].is_signer);
        assert!(!ix.accounts[0].is_writable);
        assert_eq!(ix.accounts[1].pubkey, pubkey);
        assert!(ix.accounts[1].is_writable);
        assert_eq!(
            ix.accounts[2].pubkey,
            solana_sdk_ids::sysvar::instructions::id()
        );
        assert!(!ix.accounts[2].is_writable);
        assert_eq!(ix.accounts[3].pubkey, MAGIC_CONTEXT_PUBKEY);
        assert!(ix.accounts[3].is_writable);

        match bincode::deserialize(&ix.data).unwrap() {
            PostDelegationActionExecutorInstruction::ScheduleUndelegation {
                ..
            } => {}
            _ => panic!("expected schedule undelegation instruction"),
        }
    }

    #[test]
    fn small_undelegation_clone_schedules_in_same_tx_without_actions() {
        let pubkey = Pubkey::new_unique();
        let mut request = request(pubkey, vec![1, 2, 3], vec![action()]);
        request.needs_undelegation = true;
        let tx = cloner().build_small_account_tx(&request, Hash::default());

        assert_eq!(tx.message().instructions.len(), 2);
        assert_eq!(instruction_program_id(&tx, 0), magicblock_program::ID);
        assert_eq!(
            instruction_program_id(&tx, 1),
            POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID
        );

        match bincode::deserialize(instruction_data(&tx, 0)).unwrap() {
            MagicBlockInstruction::CloneAccount {
                pubkey: clone_pubkey,
                actions,
                fields,
                ..
            } => {
                assert_eq!(clone_pubkey, pubkey);
                assert!(fields.delegated);
                assert!(actions.is_empty());
            }
            _ => panic!("expected clone account instruction"),
        }
        match bincode::deserialize(instruction_data(&tx, 1)).unwrap() {
            PostDelegationActionExecutorInstruction::ScheduleUndelegation {
                ..
            } => {}
            _ => panic!("expected schedule undelegation instruction"),
        }
    }

    #[test]
    fn large_undelegation_clone_schedules_after_final_chunk_tx() {
        let pubkey = Pubkey::new_unique();
        let mut request =
            request(pubkey, vec![7; MAX_INLINE_DATA_SIZE + 1], vec![action()]);
        request.needs_undelegation = true;
        let txs = cloner().build_large_account_txs(&request, Hash::default());

        assert_eq!(txs.len(), 3);

        let final_clone_tx = &txs[1];
        assert_eq!(final_clone_tx.message().instructions.len(), 1);
        assert_eq!(
            instruction_program_id(final_clone_tx, 0),
            magicblock_program::ID
        );

        match bincode::deserialize(instruction_data(final_clone_tx, 0)).unwrap()
        {
            MagicBlockInstruction::CloneAccountContinue {
                pubkey: continue_pubkey,
                offset,
                data,
                is_last,
                actions,
                needs_undelegation,
            } => {
                assert_eq!(continue_pubkey, pubkey);
                assert_eq!(offset, MAX_INLINE_DATA_SIZE as u32);
                assert_eq!(data, vec![7]);
                assert!(!is_last);
                assert!(actions.is_empty());
                assert!(!needs_undelegation);
            }
            _ => panic!("expected clone account continue instruction"),
        }

        let rescue_tx = txs.last().unwrap();
        assert_eq!(rescue_tx.message().instructions.len(), 2);

        assert_eq!(
            instruction_program_id(rescue_tx, 0),
            magicblock_program::ID
        );
        match bincode::deserialize(instruction_data(rescue_tx, 0)).unwrap() {
            MagicBlockInstruction::CloneAccountContinue {
                pubkey: continue_pubkey,
                offset,
                data,
                is_last,
                actions,
                needs_undelegation,
            } => {
                assert_eq!(continue_pubkey, pubkey);
                assert_eq!(offset, (MAX_INLINE_DATA_SIZE + 1) as u32);
                assert_eq!(data, Vec::<u8>::new());
                assert!(is_last);
                assert!(actions.is_empty());
                assert!(needs_undelegation);
            }
            _ => panic!("expected clone account continue instruction"),
        }

        assert_eq!(
            instruction_program_id(rescue_tx, 1),
            POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID
        );
        match bincode::deserialize(instruction_data(rescue_tx, 1)).unwrap() {
            PostDelegationActionExecutorInstruction::ScheduleUndelegation {
                ..
            } => {}
            _ => panic!("expected schedule undelegation instruction"),
        }
    }

    #[test]
    fn large_undelegation_max_final_chunk_fits() {
        let pubkey = Pubkey::new_unique();
        let mut request =
            request(pubkey, vec![7; MAX_INLINE_DATA_SIZE * 2], vec![]);
        request.needs_undelegation = true;
        let txs = cloner().build_large_account_txs(&request, Hash::default());

        ChainlinkCloner::ensure_transactions_fit(pubkey, &txs).unwrap();
    }

    #[test]
    fn chunked_undelegation_short_data_uses_empty_final_continue() {
        let pubkey = Pubkey::new_unique();
        let data = vec![1, 2, 3];
        let mut request = request(pubkey, data.clone(), vec![action()]);
        request.needs_undelegation = true;
        let txs = cloner().build_large_account_txs(&request, Hash::default());

        assert_eq!(txs.len(), 2);

        let final_clone_tx = &txs[1];
        assert_eq!(final_clone_tx.message().instructions.len(), 2);
        match bincode::deserialize(instruction_data(final_clone_tx, 0)).unwrap()
        {
            MagicBlockInstruction::CloneAccountContinue {
                pubkey: continue_pubkey,
                offset,
                data: continue_data,
                is_last,
                actions,
                needs_undelegation,
            } => {
                assert_eq!(continue_pubkey, pubkey);
                assert_eq!(offset, data.len() as u32);
                assert!(continue_data.is_empty());
                assert!(is_last);
                assert!(actions.is_empty());
                assert!(needs_undelegation);
            }
            _ => panic!("expected clone account continue instruction"),
        }

        match bincode::deserialize(instruction_data(final_clone_tx, 1)).unwrap()
        {
            PostDelegationActionExecutorInstruction::ScheduleUndelegation {
                ..
            } => {}
            _ => panic!("expected schedule undelegation instruction"),
        }
    }

    #[test]
    fn small_delegated_clone_with_actions_emits_executor_sibling() {
        let pubkey = Pubkey::new_unique();
        let actions = vec![action()];
        let tx = cloner().build_small_account_tx(
            &request(pubkey, vec![1, 2, 3], actions.clone()),
            Hash::default(),
        );

        assert_eq!(tx.message().instructions.len(), 2);
        assert_eq!(instruction_program_id(&tx, 0), magicblock_program::ID);
        assert_eq!(
            instruction_program_id(&tx, 1),
            POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID
        );

        match bincode::deserialize(instruction_data(&tx, 0)).unwrap() {
            MagicBlockInstruction::CloneAccount {
                pubkey: clone_pubkey,
                actions: clone_actions,
                fields,
                ..
            } => {
                assert_eq!(clone_pubkey, pubkey);
                assert!(fields.delegated);
                assert_eq!(clone_actions, actions);
            }
            _ => panic!("expected clone account instruction"),
        }
        match bincode::deserialize(instruction_data(&tx, 1)).unwrap() {
            PostDelegationActionExecutorInstruction::Execute {
                cloned_account_pubkey: executor_pubkey,
                actions: executor_actions,
            } => {
                assert_eq!(executor_pubkey, pubkey);
                assert_eq!(executor_actions, actions);
            }
            PostDelegationActionExecutorInstruction::ScheduleUndelegation {
                ..
            } => {
                panic!("expected execute instruction")
            }
        }
    }

    #[test]
    fn large_delegated_clone_with_actions_uses_empty_final_continue_then_executor(
    ) {
        let pubkey = Pubkey::new_unique();
        let actions = vec![action()];
        let data = vec![7; MAX_INLINE_DATA_SIZE + 1];
        let txs = cloner().build_large_account_txs(
            &request(pubkey, data, actions.clone()),
            Hash::default(),
        );

        let final_tx = txs.last().unwrap();
        assert_eq!(final_tx.message().instructions.len(), 2);
        assert_eq!(instruction_program_id(final_tx, 0), magicblock_program::ID);
        assert_eq!(
            instruction_program_id(final_tx, 1),
            POST_DELEGATION_ACTION_EXECUTOR_PROGRAM_ID
        );

        match bincode::deserialize(instruction_data(final_tx, 0)).unwrap() {
            MagicBlockInstruction::CloneAccountContinue {
                pubkey: continue_pubkey,
                offset,
                data,
                is_last,
                actions: continue_actions,
                needs_undelegation,
            } => {
                assert_eq!(continue_pubkey, pubkey);
                assert_eq!(offset, (MAX_INLINE_DATA_SIZE + 1) as u32);
                assert!(data.is_empty());
                assert!(is_last);
                assert_eq!(continue_actions, actions);
                assert!(!needs_undelegation);
            }
            _ => panic!("expected clone account continue instruction"),
        }
        match bincode::deserialize(instruction_data(final_tx, 1)).unwrap() {
            PostDelegationActionExecutorInstruction::Execute {
                cloned_account_pubkey: executor_pubkey,
                actions: executor_actions,
            } => {
                assert_eq!(executor_pubkey, pubkey);
                assert_eq!(executor_actions, actions);
            }
            PostDelegationActionExecutorInstruction::ScheduleUndelegation {
                ..
            } => {
                panic!("expected execute instruction")
            }
        }
    }
}
