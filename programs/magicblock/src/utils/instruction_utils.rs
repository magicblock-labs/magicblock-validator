use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use magicblock_magic_program_api::{
    args::ScheduleTaskArgs,
    instruction::{
        AccountModification, AccountModificationForInstruction,
        MagicBlockInstruction,
    },
    MAGIC_CONTEXT_PUBKEY,
};
use solana_program_runtime::__private::Hash;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use crate::{
    mutate_accounts::set_account_mod_data,
    validator::{validator_authority, validator_authority_id},
};

pub struct InstructionUtils;
impl InstructionUtils {
    // -----------------
    // Schedule Commit
    // -----------------
    #[cfg(test)]
    pub fn schedule_commit(
        payer: &Keypair,
        pubkeys: Vec<Pubkey>,
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix = Self::schedule_commit_instruction(&payer.pubkey(), pubkeys);
        Self::into_transaction(payer, ix, recent_blockhash)
    }

    #[cfg(test)]
    pub(crate) fn schedule_commit_instruction(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Instruction {
        schedule_commit_instruction_helper(
            payer,
            pdas,
            MagicBlockInstruction::ScheduleCommit,
        )
    }

    // -----------------
    // Schedule Compressed Commit
    // -----------------
    #[cfg(test)]
    pub fn schedule_compressed_commit(
        payer: &Keypair,
        pubkeys: Vec<Pubkey>,
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix = Self::schedule_compressed_commit_instruction(
            &payer.pubkey(),
            pubkeys,
        );
        Self::into_transaction(payer, ix, recent_blockhash)
    }

    #[cfg(test)]
    pub(crate) fn schedule_compressed_commit_instruction(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Instruction {
        schedule_commit_instruction_helper(
            payer,
            pdas,
            MagicBlockInstruction::ScheduleCompressedCommit,
        )
    }

    // -----------------
    // Schedule Commit and Undelegate
    // -----------------
    #[cfg(test)]
    pub fn schedule_commit_and_undelegate(
        payer: &Keypair,
        pubkeys: Vec<Pubkey>,
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix = Self::schedule_commit_and_undelegate_instruction(
            &payer.pubkey(),
            pubkeys,
        );
        Self::into_transaction(payer, ix, recent_blockhash)
    }

    #[cfg(test)]
    pub(crate) fn schedule_commit_and_undelegate_instruction(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Instruction {
        schedule_commit_instruction_helper(
            payer,
            pdas,
            MagicBlockInstruction::ScheduleCommitAndUndelegate,
        )
    }
    // -----------------
    // Schedule Compressed Commit and Undelegate
    // -----------------
    #[cfg(test)]
    pub fn schedule_compressed_commit_and_undelegate(
        payer: &Keypair,
        pubkeys: Vec<Pubkey>,
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix = Self::schedule_compressed_commit_and_undelegate_instruction(
            &payer.pubkey(),
            pubkeys,
        );
        Self::into_transaction(payer, ix, recent_blockhash)
    }

    #[cfg(test)]
    pub(crate) fn schedule_compressed_commit_and_undelegate_instruction(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Instruction {
        schedule_commit_instruction_helper(
            payer,
            pdas,
            MagicBlockInstruction::ScheduleCompressedCommitAndUndelegate,
        )
    }

    // -----------------
    // Scheduled Commit Sent
    // -----------------
    pub fn scheduled_commit_sent(
        scheduled_commit_id: u64,
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix = Self::scheduled_commit_sent_instruction(
            &crate::id(),
            &validator_authority_id(),
            scheduled_commit_id,
        );
        Self::into_transaction(&validator_authority(), ix, recent_blockhash)
    }

    pub(crate) fn scheduled_commit_sent_instruction(
        magic_block_program: &Pubkey,
        validator_authority: &Pubkey,
        scheduled_commit_id: u64,
    ) -> Instruction {
        static COMMIT_SENT_BUMP: AtomicU64 = AtomicU64::new(0);
        let account_metas = vec![
            AccountMeta::new_readonly(*magic_block_program, false),
            AccountMeta::new_readonly(*validator_authority, true),
        ];
        Instruction::new_with_bincode(
            *magic_block_program,
            &MagicBlockInstruction::ScheduledCommitSent((
                scheduled_commit_id,
                COMMIT_SENT_BUMP.fetch_add(1, Ordering::SeqCst),
            )),
            account_metas,
        )
    }

    // -----------------
    // Accept Scheduled Commits
    // -----------------
    pub fn accept_scheduled_commits(recent_blockhash: Hash) -> Transaction {
        let ix = Self::accept_scheduled_commits_instruction();
        Self::into_transaction(&validator_authority(), ix, recent_blockhash)
    }

    pub(crate) fn accept_scheduled_commits_instruction() -> Instruction {
        let account_metas = vec![
            AccountMeta::new_readonly(validator_authority_id(), true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        ];
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::AcceptScheduleCommits,
            account_metas,
        )
    }

    // -----------------
    // ModifyAccounts
    // -----------------
    pub fn modify_accounts(
        account_modifications: Vec<AccountModification>,
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix = Self::modify_accounts_instruction(account_modifications);
        Self::into_transaction(&validator_authority(), ix, recent_blockhash)
    }

    pub fn modify_accounts_instruction(
        account_modifications: Vec<AccountModification>,
    ) -> Instruction {
        let mut account_metas =
            vec![AccountMeta::new(validator_authority_id(), true)];
        let mut account_mods: HashMap<
            Pubkey,
            AccountModificationForInstruction,
        > = HashMap::new();
        for account_modification in account_modifications {
            account_metas
                .push(AccountMeta::new(account_modification.pubkey, false));
            let account_mod_for_instruction =
                AccountModificationForInstruction {
                    lamports: account_modification.lamports,
                    owner: account_modification.owner,
                    executable: account_modification.executable,
                    data_key: account_modification
                        .data
                        .map(set_account_mod_data),
                    rent_epoch: account_modification.rent_epoch,
                    delegated: account_modification.delegated,
                    compressed: account_modification.compressed,
                    confined: account_modification.confined,
                };
            account_mods.insert(
                account_modification.pubkey,
                account_mod_for_instruction,
            );
        }
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::ModifyAccounts(account_mods),
            account_metas,
        )
    }

    // -----------------
    // Schedule Task
    // -----------------
    pub fn schedule_task(
        payer: &Keypair,
        args: ScheduleTaskArgs,
        accounts: &[Pubkey],
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix =
            Self::schedule_task_instruction(&payer.pubkey(), args, accounts);
        Self::into_transaction(payer, ix, recent_blockhash)
    }

    pub fn schedule_task_instruction(
        payer: &Pubkey,
        args: ScheduleTaskArgs,
        accounts: &[Pubkey],
    ) -> Instruction {
        let mut account_metas = vec![AccountMeta::new(*payer, true)];
        for account in accounts {
            account_metas.push(AccountMeta::new_readonly(*account, false));
        }

        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleTask(args),
            account_metas,
        )
    }

    // -----------------
    // Cancel Task
    // -----------------
    pub fn cancel_task(
        authority: &Keypair,
        task_id: i64,
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix = Self::cancel_task_instruction(&authority.pubkey(), task_id);
        Self::into_transaction(authority, ix, recent_blockhash)
    }

    pub fn cancel_task_instruction(
        authority: &Pubkey,
        task_id: i64,
    ) -> Instruction {
        let account_metas = vec![AccountMeta::new(*authority, true)];

        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CancelTask { task_id },
            account_metas,
        )
    }

    // -----------------
    // Executable Check
    // -----------------
    pub fn disable_executable_check_instruction(
        authority: &Pubkey,
    ) -> Instruction {
        let account_metas = vec![AccountMeta::new(*authority, true)];

        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::DisableExecutableCheck,
            account_metas,
        )
    }

    pub fn enable_executable_check_instruction(
        authority: &Pubkey,
    ) -> Instruction {
        let account_metas = vec![AccountMeta::new(*authority, true)];

        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::EnableExecutableCheck,
            account_metas,
        )
    }

    // -----------------
    // Utils
    // -----------------
    pub(crate) fn into_transaction(
        signer: &Keypair,
        instruction: Instruction,
        recent_blockhash: Hash,
    ) -> Transaction {
        let signers = &[&signer];
        Transaction::new_signed_with_payer(
            &[instruction],
            Some(&signer.pubkey()),
            signers,
            recent_blockhash,
        )
    }
}

/// Schedule commit instructions use exactly the same accounts
#[cfg(test)]
fn schedule_commit_instruction_helper(
    payer: &Pubkey,
    pdas: Vec<Pubkey>,
    instruction: MagicBlockInstruction,
) -> Instruction {
    let mut account_metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
    ];
    let is_undelegation = matches!(
        instruction,
        MagicBlockInstruction::ScheduleCommitAndUndelegate
            | MagicBlockInstruction::ScheduleCompressedCommitAndUndelegate
    );
    for pubkey in &pdas {
        if is_undelegation {
            account_metas.push(AccountMeta::new(*pubkey, false));
        } else {
            account_metas.push(AccountMeta::new_readonly(*pubkey, true));
        }
    }
    Instruction::new_with_bincode(crate::id(), &instruction, account_metas)
}
