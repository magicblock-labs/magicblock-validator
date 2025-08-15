use magicblock_core::magic_program::args::MagicBaseIntentArgs;
use solana_sdk::instruction::InstructionError;

use crate::{
    magic_scheduled_base_intent::{CommitType, ConstructionContext},
    utils::account_actions::set_account_owner_to_delegation_program,
};

pub fn schedule_base_intent_processor(
    construction_context: &ConstructionContext<'_, '_>,
    args: &MagicBaseIntentArgs,
) -> Result<(), InstructionError> {
    let commited_accounts_ref = match args {
        MagicBaseIntentArgs::Commit(commit_type) => {
            let accounts_indices = commit_type.committed_accounts_indices();
            CommitType::extract_commit_accounts(
                accounts_indices,
                construction_context.transaction_context,
            )?
        }
        MagicBaseIntentArgs::CommitAndUndelegate(
            commit_and_undelegate_type,
        ) => {
            let accounts_indices =
                commit_and_undelegate_type.committed_accounts_indices();
            CommitType::extract_commit_accounts(
                accounts_indices,
                construction_context.transaction_context,
            )?
        }
        MagicBaseIntentArgs::BaseActions(_) => return Ok(()),
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
