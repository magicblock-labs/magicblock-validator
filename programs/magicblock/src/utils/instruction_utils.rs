use magicblock_core::intent::outbox::outbox_intent_pda;
use magicblock_magic_program_api::{
    CRANK_PROGRAM_ID, MAGIC_CONTEXT_PUBKEY, args::ScheduleTaskArgs,
    instruction::MagicBlockInstruction, outbox, pda::crank_signer_pda,
};
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::validator::{validator_authority, validator_authority_id};

/// Builders for the MagicBlock program instructions.
///
/// Most builders return bare [`Instruction`]s for the engine to compose and
/// sign. `scheduled_commit_sent` is pre-signed while scheduling so its future
/// signature can be returned in the scheduling transaction logs.
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

    pub fn schedule_commit_instruction(
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
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleCommit,
            account_metas,
        )
    }

    // -----------------
    // Schedule Commit and Undelegate
    // -----------------
    pub fn schedule_commit_and_undelegate_instruction(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Instruction {
        let mut account_metas = vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        ];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new(*pubkey, true));
        }
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleCommitAndUndelegate,
            account_metas,
        )
    }

    /// Builds a top-level validator-authorized commit-and-undelegate request.
    ///
    /// This is structurally similar to
    /// `schedule_commit_and_undelegate_instruction`, but with important
    /// authorization differences: the validator authority is the signer, and
    /// delegated accounts are writable non-signers.
    ///
    /// In `schedule_commit_and_undelegate_instruction`, each delegated account
    /// is marked as a signer because that builder is used by the owner-program
    /// CPI path, where the owner program authorizes the PDA through CPI. The
    /// undelegation request service observes a DLP request and submits a
    /// validator-internal transaction instead, so the delegated account PDA
    /// cannot sign. The Magic Program accepts the validator authority signer as
    /// authorization, while each delegated account still has to be writable so
    /// the processor can mark it as undelegating.
    pub fn validator_schedule_commit_and_undelegate_instruction(
        validator_authority: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Instruction {
        let mut account_metas = vec![
            AccountMeta::new(*validator_authority, true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        ];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new(*pubkey, false));
        }
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleCommitAndUndelegate,
            account_metas,
        )
    }

    #[cfg(test)]
    pub(crate) fn schedule_commit_with_delegated_payer_instruction(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Instruction {
        let fee_vault = crate::schedule_transactions::magic_fee_vault_pubkey();
        let mut account_metas = vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
            AccountMeta::new(fee_vault, false),
        ];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new_readonly(*pubkey, true));
        }
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleCommit,
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
        let ix = Self::scheduled_commit_sent_instruction(scheduled_commit_id);
        Self::into_transaction(&validator_authority(), ix, recent_blockhash)
    }

    pub(crate) fn scheduled_commit_sent_instruction(
        scheduled_commit_id: u64,
    ) -> Instruction {
        let account_metas = vec![
            AccountMeta::new(validator_authority_id(), true),
            AccountMeta::new_readonly(crate::id(), false),
            AccountMeta::new(outbox_intent_pda(scheduled_commit_id), false),
        ];
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::ScheduledCommitSent(scheduled_commit_id),
            account_metas,
        )
    }

    // -----------------
    // Close Outbox Intent
    // -----------------
    pub fn close_outbox_intent(
        intent_id: u64,
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix = Self::close_outbox_intent_instruction(intent_id);
        Self::into_transaction(&validator_authority(), ix, recent_blockhash)
    }

    pub(crate) fn close_outbox_intent_instruction(
        intent_id: u64,
    ) -> Instruction {
        let account_metas = vec![
            AccountMeta::new(validator_authority_id(), true),
            AccountMeta::new_readonly(crate::id(), false),
            AccountMeta::new(outbox_intent_pda(intent_id), false),
        ];
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::CloseOutboxIntent(intent_id),
            account_metas,
        )
    }

    // -----------------
    // Accept Scheduled Commits
    // -----------------
    pub fn accept_scheduled_commits(
        recent_blockhash: Hash,
        intent_ids: impl IntoIterator<Item = u64>,
    ) -> Transaction {
        let ix = Self::accept_scheduled_commits_instruction(intent_ids);
        Self::into_transaction(&validator_authority(), ix, recent_blockhash)
    }

    pub fn accept_scheduled_commits_instruction(
        intent_ids: impl IntoIterator<Item = u64>,
    ) -> Instruction {
        let mut account_metas = vec![
            AccountMeta::new(validator_authority_id(), true),
            AccountMeta::new_readonly(crate::id(), false),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        ];

        // Add outbox intent accounts
        let outbox_intent_metas = intent_ids
            .into_iter()
            .map(outbox_intent_pda)
            .map(|intent_pda| AccountMeta::new(intent_pda, false));
        account_metas.extend(outbox_intent_metas);

        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::AcceptScheduleCommits,
            account_metas,
        )
    }

    // -----------------
    // SetIntentExecutionStage
    // -----------------

    pub fn set_intent_execution_stage(
        recent_blockhash: Hash,
        intent_id: u64,
        stage: outbox::ExecutionStage,
    ) -> Transaction {
        let ix = Self::set_intent_execution_stage_instruction(intent_id, stage);
        Self::into_transaction(&validator_authority(), ix, recent_blockhash)
    }

    pub(crate) fn set_intent_execution_stage_instruction(
        intent_id: u64,
        stage: outbox::ExecutionStage,
    ) -> Instruction {
        let account_metas = vec![
            AccountMeta::new_readonly(validator_authority_id(), true),
            AccountMeta::new(outbox_intent_pda(intent_id), false),
        ];
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::SetIntentExecutionStage {
                intent_id,
                stage,
            },
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
        let signers = &[signer];
        Transaction::new_signed_with_payer(
            &[instruction],
            Some(&signer.pubkey()),
            signers,
            recent_blockhash,
        )
    }

    // -----------------
    // Schedule Task
    // -----------------
    pub fn schedule_task_instruction(
        payer: &Pubkey,
        args: ScheduleTaskArgs,
    ) -> Instruction {
        let account_metas = vec![AccountMeta::new(*payer, true)];

        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleTask(args),
            account_metas,
        )
    }

    // -----------------
    // Cancel Task
    // -----------------
    pub fn cancel_task_instruction(
        authority: &Pubkey,
        task_id: i64,
    ) -> Instruction {
        let account_metas = vec![AccountMeta::new(*authority, true)];

        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::CancelTask { task_id },
            account_metas,
        )
    }

    // -----------------
    // Execute Crank
    // -----------------
    pub fn execute_task_instruction(
        validator_authority: Pubkey,
        authority: Pubkey,
        instructions: Vec<Instruction>,
    ) -> Instruction {
        let mut account_metas = vec![
            AccountMeta::new_readonly(validator_authority, true),
            AccountMeta::new_readonly(crank_signer_pda(&authority), false),
        ];
        for instruction in &instructions {
            account_metas
                .push(AccountMeta::new_readonly(instruction.program_id, false));
            account_metas.extend(instruction.accounts.iter().map(|account| {
                AccountMeta {
                    pubkey: account.pubkey,
                    is_signer: false,
                    is_writable: account.is_writable,
                }
            }));
        }
        Instruction::new_with_wincode(
            CRANK_PROGRAM_ID,
            &MagicBlockInstruction::ExecuteCrank {
                authority,
                instructions,
            },
            account_metas,
        )
    }

    // -----------------
    // Noop
    // -----------------
    pub fn noop_instruction(data: u64) -> Instruction {
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::Noop(data),
            vec![],
        )
    }
}
