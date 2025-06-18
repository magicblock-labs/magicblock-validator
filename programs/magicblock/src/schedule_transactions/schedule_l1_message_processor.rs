use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{
    instruction::InstructionError, transaction_context::TransactionContext,
};

use crate::{
    args::MagicL1MessageArgs,
    magic_schedule_l1_message::{CommitType, ConstructionContext},
    utils::account_actions::set_account_owner_to_delegation_program,
};

pub fn process_scheddule_l1_message<'a, 'ic>(
    construction_context: &ConstructionContext<'a, 'ic>,
    args: &MagicL1MessageArgs,
) -> Result<(), InstructionError> {
    let commited_accounts_ref = match args {
        MagicL1MessageArgs::Commit(commit_type) => {
            let accounts_indices = commit_type.committed_accounts_indices();
            CommitType::extract_commit_accounts(
                accounts_indices,
                construction_context.transaction_context,
            )?
        }
        MagicL1MessageArgs::CommitAndUndelegate(commit_and_undelegate_type) => {
            let accounts_indices =
                commit_and_undelegate_type.committed_accounts_indices();
            CommitType::extract_commit_accounts(
                accounts_indices,
                construction_context.transaction_context,
            )?
        }
        MagicL1MessageArgs::L1Actions(_) => return Ok(()),
    };

    // TODO: proper explanation
    // Change owner to dlp
    commited_accounts_ref
        .into_iter()
        .for_each(|(_, account_ref)| {
            set_account_owner_to_delegation_program(account_ref);
        });

    Ok(())
}
