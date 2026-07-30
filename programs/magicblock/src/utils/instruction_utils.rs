use std::sync::atomic::{AtomicU64, Ordering};

use magicblock_magic_program_api::{
    MAGIC_CONTEXT_PUBKEY, args::ScheduleTaskArgs,
    instruction::MagicBlockInstruction,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

/// Builders for the MagicBlock program instructions.
///
/// These return bare [`Instruction`]s only. Composing them into a transaction
/// and signing is the engine's responsibility: validator-internal instructions
/// are submitted through `engine.transaction(message)`, where the engine signs
/// with its own authority — the same identity the builtins observe through
/// [`crate::validator::authority`].
pub struct InstructionUtils;
impl InstructionUtils {
    // -----------------
    // Schedule Commit
    // -----------------
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
    pub fn scheduled_commit_sent_instruction(
        magic_block_program: &Pubkey,
        validator_authority: &Pubkey,
        scheduled_commit_id: u64,
    ) -> Instruction {
        static COMMIT_SENT_BUMP: AtomicU64 = AtomicU64::new(0);
        let account_metas = vec![
            AccountMeta::new_readonly(*magic_block_program, false),
            AccountMeta::new_readonly(*validator_authority, true),
        ];
        Instruction::new_with_wincode(
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
    pub fn accept_scheduled_commits_instruction(
        validator_authority: &Pubkey,
    ) -> Instruction {
        let account_metas = vec![
            AccountMeta::new_readonly(*validator_authority, true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        ];
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::AcceptScheduleCommits,
            account_metas,
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
