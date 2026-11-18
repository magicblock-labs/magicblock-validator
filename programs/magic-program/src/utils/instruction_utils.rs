use magic_program_api::{
    MAGIC_CONTEXT_PUBKEY, OUTBOX_INTENT_PROGRAM_ID,
    args::ScheduleTaskArgs,
    instruction::{MagicBlockInstruction, OutboxIntentInstruction},
    outbox::{self, SentCommit},
};
use mbv_core::intent::outbox::outbox_intent_pda;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

#[cfg(test)]
use {
    solana_hash::Hash, solana_keypair::Keypair, solana_signer::Signer,
    solana_transaction::Transaction,
};

/// Builders for the MagicBlock program instructions.
///
/// Builders take the authority pubkey explicitly; host code signs outside
/// program execution.
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
        Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], recent_blockhash)
    }

    pub fn schedule_commit_instruction(payer: &Pubkey, pdas: Vec<Pubkey>) -> Instruction {
        let mut account_metas =
            vec![AccountMeta::new(*payer, true), AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false)];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new(*pubkey, true));
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
        let mut account_metas =
            vec![AccountMeta::new(*payer, true), AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false)];
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
            account_metas.push(AccountMeta::new(*pubkey, true));
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
        validator: Pubkey,
        receipt: SentCommit,
    ) -> Result<Instruction, wincode::WriteError> {
        let intent_id = receipt.intent_id;
        let data = wincode::serialize(&OutboxIntentInstruction::ScheduledCommitSent(receipt))?;
        // Solana's signing path unwraps message serialization. Reject data that
        // cannot fit its u16 instruction length before handing it to Engine.
        if data.len() > u16::MAX as usize {
            return Err(wincode::WriteError::LengthEncodingOverflow("u16::MAX"));
        }
        let account_metas = vec![
            AccountMeta::new_readonly(validator, true),
            AccountMeta::new(outbox_intent_pda(intent_id), false),
        ];
        Ok(Instruction {
            program_id: OUTBOX_INTENT_PROGRAM_ID,
            data,
            accounts: account_metas,
        })
    }

    // -----------------
    // Close Outbox Intent
    // -----------------
    #[cfg(test)]
    pub(crate) fn close_outbox_intent_instruction(
        validator: Pubkey,
        intent_id: u64,
    ) -> Instruction {
        let account_metas = vec![
            AccountMeta::new_readonly(validator, true),
            AccountMeta::new(outbox_intent_pda(intent_id), false),
        ];
        Instruction::new_with_wincode(
            OUTBOX_INTENT_PROGRAM_ID,
            &OutboxIntentInstruction::CloseOutboxIntent(intent_id),
            account_metas,
        )
    }

    // -----------------
    // Accept Scheduled Commits
    // -----------------
    pub fn accept_scheduled_commits_instruction(
        validator: Pubkey,
        intent_ids: impl IntoIterator<Item = u64>,
    ) -> Instruction {
        let mut account_metas = vec![
            AccountMeta::new_readonly(validator, true),
            AccountMeta::new_readonly(OUTBOX_INTENT_PROGRAM_ID, false),
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

    pub fn set_intent_execution_stage_instruction(
        validator: Pubkey,
        intent_id: u64,
        stage: outbox::ExecutionStage,
        recovery_commit_nonces: Vec<(Pubkey, u64)>,
    ) -> Instruction {
        let account_metas = vec![
            AccountMeta::new_readonly(validator, true),
            AccountMeta::new(outbox_intent_pda(intent_id), false),
        ];
        Instruction::new_with_wincode(
            OUTBOX_INTENT_PROGRAM_ID,
            &OutboxIntentInstruction::SetIntentExecutionStage {
                intent_id,
                stage,
                recovery_commit_nonces,
            },
            account_metas,
        )
    }

    // -----------------
    // Schedule Task
    // -----------------
    pub fn schedule_task_instruction(payer: &Pubkey, args: ScheduleTaskArgs) -> Instruction {
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
    pub fn cancel_task_instruction(authority: &Pubkey, task_id: i64) -> Instruction {
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
        Instruction::new_with_wincode(crate::id(), &MagicBlockInstruction::Noop(data), vec![])
    }
}
