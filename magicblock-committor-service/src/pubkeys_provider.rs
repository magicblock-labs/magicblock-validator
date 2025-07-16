use std::collections::HashSet;

use dlp::pda;
use log::*;
use solana_pubkey::Pubkey;
use solana_sdk::system_program;

/// Returns all accounts needed to process/finalize a commit for the account
/// with the provided `delegated_account`.
/// NOTE: that buffer and chunk accounts are different for each commit and
///       thus are not included
pub fn provide_committee_pubkeys(
    committee: &Pubkey,
    owner_program: Option<&Pubkey>,
) -> HashSet<Pubkey> {
    let mut set = HashSet::new();
    set.insert(*committee);
    set.insert(pda::delegation_record_pda_from_delegated_account(committee));
    set.insert(pda::delegation_metadata_pda_from_delegated_account(
        committee,
    ));
    set.insert(pda::commit_state_pda_from_delegated_account(committee));
    set.insert(pda::commit_record_pda_from_delegated_account(committee));
    set.insert(pda::undelegate_buffer_pda_from_delegated_account(committee));

    // NOTE: ideally we'd also include the rent_fee_payer here, but that is
    //       not known to the validator at the time of cloning since it is
    //       stored inside the delegation metadata account instead of the
    //       delegation record

    if let Some(owner_program) = owner_program {
        set.insert(pda::program_config_from_program_id(owner_program));
    } else {
        warn!(
            "No owner program provided for committee pubkey {}",
            committee
        );
    }
    set
}

/// Returns common accounts needed for process/finalize transactions,
/// namely the program ids used and the fees vaults and the validator itself.
pub fn provide_common_pubkeys(validator: &Pubkey) -> HashSet<Pubkey> {
    let mut set = HashSet::new();

    let deleg_program = dlp::id();
    let protocol_fees_vault = pda::fees_vault_pda();
    let validator_fees_vault =
        pda::validator_fees_vault_pda_from_validator(validator);
    let committor_program = magicblock_committor_program::id();

    trace!(
        "Common pubkeys:
    validator:                {}
    delegation program:       {}
    protocol fees vault:      {}
    validator fees vault:     {}
    committor program:        {}",
        validator,
        deleg_program,
        protocol_fees_vault,
        validator_fees_vault,
        committor_program
    );

    set.insert(*validator);
    set.insert(system_program::id());
    set.insert(deleg_program);
    set.insert(protocol_fees_vault);
    set.insert(validator_fees_vault);
    set.insert(committor_program);

    set
}
