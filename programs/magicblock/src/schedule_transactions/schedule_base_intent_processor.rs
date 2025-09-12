use magicblock_magic_program_api::args::MagicBaseIntentArgs;
use solana_sdk::instruction::InstructionError;

use crate::{
    magic_scheduled_base_intent::{CommitType, ConstructionContext},
    utils::account_actions::set_account_owner_to_delegation_program,
};

pub fn change_owner_for_undelegated_accounts(
    construction_context: &ConstructionContext<'_, '_>,
    args: &MagicBaseIntentArgs,
) -> Result<(), InstructionError> {
    let commited_accounts_ref = match args {
        MagicBaseIntentArgs::Commit(_) => {
            return Ok(());
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

    // Change owner to dlp
    // Once account is undelegated we need to make it immutable in our validator.
    commited_accounts_ref
        .into_iter()
        .for_each(|(_, account_ref)| {
            set_account_owner_to_delegation_program(account_ref);
        });

    Ok(())
}
