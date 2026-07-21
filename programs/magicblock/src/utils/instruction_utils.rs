use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use magicblock_magic_program_api::{
    CRANK_PROGRAM_ID, MAGIC_CONTEXT_PUBKEY,
    args::ScheduleTaskArgs,
    instruction::{
        AccountModification, AccountModificationForInstruction,
        MagicBlockInstruction,
    },
    pda::crank_signer_pda,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::validator::authority;

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
    // ModifyAccounts
    // -----------------

    pub fn modify_accounts_instruction(
        account_modifications: Vec<AccountModification>,
        message: Option<String>,
    ) -> Instruction {
        let mut account_metas = vec![AccountMeta::new(authority(), true)];
        let mut account_mods: HashMap<
            Pubkey,
            AccountModificationForInstruction,
        > = HashMap::new();
        for account_modification in account_modifications {
            account_metas
                .push(AccountMeta::new(account_modification.pubkey, false));
            let account_mod_for_instruction =
                AccountModificationForInstruction {
                    owner: account_modification.owner,
                    delegated: account_modification.delegated,
                    confined: account_modification.confined,
                };
            account_mods.insert(
                account_modification.pubkey,
                account_mod_for_instruction,
            );
        }
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::ModifyAccounts {
                accounts: account_mods,
                message,
            },
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
    // Executable Check
    // -----------------
    pub fn disable_executable_check_instruction(
        authority: &Pubkey,
    ) -> Instruction {
        let account_metas = vec![AccountMeta::new(*authority, true)];

        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::DisableExecutableCheck,
            account_metas,
        )
    }

    pub fn enable_executable_check_instruction(
        authority: &Pubkey,
    ) -> Instruction {
        let account_metas = vec![AccountMeta::new(*authority, true)];

        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::EnableExecutableCheck,
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

    // -----------------
    // EvictAccount
    // -----------------
    pub fn evict_account_instruction(pubkey: Pubkey) -> Instruction {
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::EvictAccount { pubkey },
            vec![
                AccountMeta::new(authority(), true),
                AccountMeta::new(pubkey, false),
            ],
        )
    }

    // -----------------
    // CloneAccount
    // -----------------
    fn append_action_accounts(
        account_metas: &mut Vec<AccountMeta>,
        actions: &[Instruction],
    ) {
        for action in actions {
            Self::push_or_update_account_meta(
                account_metas,
                AccountMeta::new_readonly(action.program_id, false),
            );
            for account in &action.accounts {
                let mut account = account.clone();
                account.is_signer = false;
                Self::push_or_update_account_meta(account_metas, account);
            }
        }
    }

    fn push_or_update_account_meta(
        account_metas: &mut Vec<AccountMeta>,
        account_meta: AccountMeta,
    ) {
        if let Some(existing) = account_metas
            .iter_mut()
            .find(|existing| existing.pubkey == account_meta.pubkey)
        {
            existing.is_writable |= account_meta.is_writable;
            existing.is_signer |= account_meta.is_signer;
            return;
        }
        account_metas.push(account_meta);
    }

    fn append_instructions_sysvar_account(
        account_metas: &mut Vec<AccountMeta>,
        actions: &[Instruction],
    ) {
        if !actions.is_empty() {
            Self::push_or_update_account_meta(
                account_metas,
                AccountMeta::new_readonly(
                    solana_sdk_ids::sysvar::instructions::id(),
                    false,
                ),
            );
        }
    }

    pub fn clone_account_instruction(
        pubkey: Pubkey,
        data: Vec<u8>,
        fields: magicblock_magic_program_api::instruction::AccountCloneFields,
        actions: Vec<Instruction>,
    ) -> Instruction {
        let mut account_metas = vec![
            AccountMeta::new(authority(), true),
            AccountMeta::new(pubkey, false),
        ];
        Self::append_instructions_sysvar_account(&mut account_metas, &actions);
        Self::append_action_accounts(&mut account_metas, &actions);
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::CloneAccount {
                pubkey,
                data,
                fields,
                actions,
            },
            account_metas,
        )
    }

    pub fn clone_account_init_instruction(
        pubkey: Pubkey,
        total_data_len: u32,
        initial_data: Vec<u8>,
        fields: magicblock_magic_program_api::instruction::AccountCloneFields,
    ) -> Instruction {
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::CloneAccountInit {
                pubkey,
                total_data_len,
                initial_data,
                fields,
            },
            vec![
                AccountMeta::new(authority(), true),
                AccountMeta::new(pubkey, false),
            ],
        )
    }

    pub fn clone_account_continue_instruction(
        pubkey: Pubkey,
        offset: u32,
        data: Vec<u8>,
        is_last: bool,
        actions: Vec<Instruction>,
        needs_undelegation: bool,
    ) -> Instruction {
        let mut account_metas = vec![
            AccountMeta::new(authority(), true),
            AccountMeta::new(pubkey, false),
        ];
        Self::append_instructions_sysvar_account(&mut account_metas, &actions);
        Self::append_action_accounts(&mut account_metas, &actions);
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::CloneAccountContinue {
                pubkey,
                offset,
                data,
                is_last,
                actions,
                needs_undelegation,
            },
            account_metas,
        )
    }

    pub fn cleanup_partial_clone_instruction(pubkey: Pubkey) -> Instruction {
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::CleanupPartialClone { pubkey },
            vec![
                AccountMeta::new(authority(), true),
                AccountMeta::new(pubkey, false),
            ],
        )
    }

    // -----------------
    // Program Cloning
    // -----------------
    pub fn finalize_program_from_buffer_instruction(
        program: Pubkey,
        buffer: Pubkey,
        remote_slot: u64,
    ) -> Instruction {
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::FinalizeProgramFromBuffer { remote_slot },
            vec![
                AccountMeta::new_readonly(authority(), true),
                AccountMeta::new(program, false),
                AccountMeta::new(buffer, false),
            ],
        )
    }

    pub fn finalize_v1_program_from_buffer_instruction(
        program: Pubkey,
        program_data: Pubkey,
        buffer: Pubkey,
        remote_slot: u64,
        authority_pubkey: Pubkey,
    ) -> Instruction {
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::FinalizeV1ProgramFromBuffer {
                remote_slot,
                authority: authority_pubkey,
            },
            vec![
                AccountMeta::new_readonly(authority(), true),
                AccountMeta::new(program, false),
                AccountMeta::new(program_data, false),
                AccountMeta::new(buffer, false),
            ],
        )
    }

    pub fn set_program_authority_instruction(
        program: Pubkey,
        authority_pubkey: Pubkey,
    ) -> Instruction {
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::SetProgramAuthority {
                authority: authority_pubkey,
            },
            vec![
                AccountMeta::new_readonly(authority(), true),
                AccountMeta::new(program, false),
            ],
        )
    }
}
