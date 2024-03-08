use log::*;
use solana_sdk::{
    account::Account,
    account_utils::StateMut,
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    loader_v4::{self, LoaderV4State},
    pubkey::Pubkey,
};

use crate::errors::{MutatorError, MutatorResult};

pub fn adjust_deployment_slot(
    program_address: &Pubkey,
    programdata_address: &Pubkey,
    program_account: &Account,
    programdata_account: Option<&mut Account>,
    deployment_slot: u64,
) -> MutatorResult<()> {
    if loader_v4::check_id(&program_account.owner) {
        if let Ok(data) = solana_loader_v4_program::get_state(&program_account.data) {
            let LoaderV4State {
                slot: _,
                authority_address: _,
                status: _,
            } = data;
            // TODO: figure out how to set state (only a get_state method exists)
            // solana/svm/src/transaction_processor.rs :817
            return Err(MutatorError::NotYetSupportingCloningSolanaLoader4Programs);
        }
    }

    if !bpf_loader_upgradeable::check_id(&program_account.owner) {
        // ProgramOfLoaderV1orV2 has no deployment state as part the program data
        return Ok(());
    }

    if let UpgradeableLoaderState::Program {
        programdata_address,
    } = program_account.state()?
    {
        match programdata_account {
            Some(programdata_account) => {
                if let UpgradeableLoaderState::ProgramData {
                    slot: slot_on_cluster,
                    upgrade_authority_address,
                } = programdata_account.state()?
                {
                    let metadata = UpgradeableLoaderState::ProgramData {
                        slot: deployment_slot,
                        upgrade_authority_address,
                    };
                    trace!(
                        "Change slot for ProgramData at: '{}' from {} to {}",
                        programdata_address,
                        slot_on_cluster,
                        deployment_slot
                    );
                    programdata_account.set_state(&metadata)?;
                    Ok(())
                } else {
                    Err(MutatorError::InvalidExecutableDataAccountData(
                        program_address.to_string(),
                        programdata_address.to_string(),
                    ))
                }
            }
            None => Err(
                MutatorError::NoProgramDataAccountProvidedForUpgradeableLoaderProgram(
                    program_address.to_string(),
                ),
            ),
        }
    } else {
        Err(MutatorError::InvalidExecutableDataAccountData(
            program_address.to_string(),
            programdata_address.to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use solana_sdk::{native_token::LAMPORTS_PER_SOL, pubkey::Pubkey};
    use test_tools::init_logger;

    use crate::get_executable_address;

    use super::*;

    #[test]
    fn upgradable_loader_program_slot() {
        init_logger!();

        let upgrade_authority = Pubkey::new_unique();
        let program_addr = Pubkey::new_unique();
        let programdata_address = get_executable_address(&program_addr.to_string()).unwrap();

        let program_data = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let deployment_slot = 9999;

        let program_account = {
            let data = bincode::serialize(&UpgradeableLoaderState::Program {
                programdata_address,
            })
            .unwrap();
            Account {
                lamports: LAMPORTS_PER_SOL,
                owner: bpf_loader_upgradeable::id(),
                data,
                executable: true,
                rent_epoch: u64::MAX,
            }
        };

        let mut programdata_account = {
            let mut data = bincode::serialize(&UpgradeableLoaderState::ProgramData {
                slot: deployment_slot,
                upgrade_authority_address: Some(upgrade_authority),
            })
            .unwrap();
            data.extend_from_slice(&program_data);

            Account {
                lamports: LAMPORTS_PER_SOL,
                owner: bpf_loader_upgradeable::id(),
                data,
                executable: false,
                rent_epoch: u64::MAX,
            }
        };

        let adjust_slot = 1000;
        adjust_deployment_slot(
            &program_addr,
            &programdata_address,
            &program_account,
            Some(&mut programdata_account),
            adjust_slot,
        )
        .unwrap();

        let programdata_meta: UpgradeableLoaderState = programdata_account.state().unwrap();
        let programdata_data = programdata_account.data
            [UpgradeableLoaderState::size_of_programdata_metadata()..]
            .to_vec();

        // UpgradeAuthority is not changed, but slot is adjusted
        assert_matches!(
            programdata_meta,
            UpgradeableLoaderState::ProgramData {
                slot: s,
                upgrade_authority_address: a,
            } => {
                assert_eq!(s, adjust_slot);
                assert_eq!(a, Some(upgrade_authority));
            }
        );
        // Executable data is unchanged
        assert_eq!(programdata_data, program_data);
    }
}
