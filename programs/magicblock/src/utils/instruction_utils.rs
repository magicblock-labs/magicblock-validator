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
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::validator::{validator_authority, validator_authority_id};

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
            account_metas.push(AccountMeta::new(*pubkey, true));
        }
        Instruction::new_with_bincode(
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
        Instruction::new_with_bincode(
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
        message: Option<String>,
        recent_blockhash: Hash,
    ) -> Transaction {
        let ix =
            Self::modify_accounts_instruction(account_modifications, message);
        Self::into_transaction(&validator_authority(), ix, recent_blockhash)
    }

    pub fn modify_accounts_instruction(
        account_modifications: Vec<AccountModification>,
        message: Option<String>,
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
                    data: account_modification.data,
                    delegated: account_modification.delegated,
                    confined: account_modification.confined,
                    remote_slot: account_modification.remote_slot,
                };
            account_mods.insert(
                account_modification.pubkey,
                account_mod_for_instruction,
            );
        }
        Instruction::new_with_bincode(
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
    // Noop
    // -----------------
    pub fn noop_instruction(data: u64) -> Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::Noop(data),
            vec![],
        )
    }

    // -----------------
    // CloneAccount
    // -----------------
    pub fn clone_account_instruction(
        pubkey: Pubkey,
        data: Vec<u8>,
        fields: magicblock_magic_program_api::instruction::AccountCloneFields,
    ) -> Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CloneAccount {
                pubkey,
                data,
                fields,
            },
            vec![
                AccountMeta::new(validator_authority_id(), true),
                AccountMeta::new(pubkey, false),
            ],
        )
    }

    pub fn clone_account_init_instruction(
        pubkey: Pubkey,
        total_data_len: u32,
        initial_data: Vec<u8>,
        fields: magicblock_magic_program_api::instruction::AccountCloneFields,
    ) -> Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CloneAccountInit {
                pubkey,
                total_data_len,
                initial_data,
                fields,
            },
            vec![
                AccountMeta::new(validator_authority_id(), true),
                AccountMeta::new(pubkey, false),
            ],
        )
    }

    pub fn clone_account_continue_instruction(
        pubkey: Pubkey,
        offset: u32,
        data: Vec<u8>,
        is_last: bool,
    ) -> Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CloneAccountContinue {
                pubkey,
                offset,
                data,
                is_last,
            },
            vec![
                AccountMeta::new(validator_authority_id(), true),
                AccountMeta::new(pubkey, false),
            ],
        )
    }

    pub fn cleanup_partial_clone_instruction(pubkey: Pubkey) -> Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CleanupPartialClone { pubkey },
            vec![
                AccountMeta::new(validator_authority_id(), true),
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
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::FinalizeProgramFromBuffer { remote_slot },
            vec![
                AccountMeta::new_readonly(validator_authority_id(), true),
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
        authority: Pubkey,
    ) -> Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::FinalizeV1ProgramFromBuffer {
                remote_slot,
                authority,
            },
            vec![
                AccountMeta::new_readonly(validator_authority_id(), true),
                AccountMeta::new(program, false),
                AccountMeta::new(program_data, false),
                AccountMeta::new(buffer, false),
            ],
        )
    }

    pub fn set_program_authority_instruction(
        program: Pubkey,
        authority: Pubkey,
    ) -> Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::SetProgramAuthority { authority },
            vec![
                AccountMeta::new_readonly(validator_authority_id(), true),
                AccountMeta::new(program, false),
            ],
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
