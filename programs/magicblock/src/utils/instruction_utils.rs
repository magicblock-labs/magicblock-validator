use std::collections::HashMap;

use magicblock_core::magic_program::{
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
        let mut account_metas = vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        ];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new_readonly(*pubkey, true));
        }
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleCommit,
            account_metas,
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
        let mut account_metas = vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        ];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new_readonly(*pubkey, true));
        }
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleCommitAndUndelegate,
            account_metas,
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
        let account_metas = vec![
            AccountMeta::new_readonly(*magic_block_program, false),
            AccountMeta::new_readonly(*validator_authority, true),
        ];
        Instruction::new_with_bincode(
            *magic_block_program,
            &MagicBlockInstruction::ScheduledCommitSent(scheduled_commit_id),
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
