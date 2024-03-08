use log::*;
use solana_sdk::{
    account::Account,
    account_utils::StateMut,
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    loader_v4::{self, LoaderV4State},
};

pub fn adjust_deployment_slot(
    program_account: &mut Account,
    programdata_account: &mut Option<Account>,
    deployment_slot: u64,
) {
    if loader_v4::check_id(&program_account.owner) {
        if let Ok(data) = solana_loader_v4_program::get_state(&program_account.data) {
            let LoaderV4State {
                slot: _,
                authority_address: _,
                status: _,
            } = data;
            todo!("Handle solana_loader_v4_program");
        }
    }

    if !bpf_loader_upgradeable::check_id(&program_account.owner) {
        // solana/svm/src/transaction_processor.rs :830
        todo!("Handle ProgramOfLoaderV1orV2");
    }

    if let Ok(UpgradeableLoaderState::Program {
        programdata_address,
    }) = program_account.state()
    {
        match programdata_account {
            Some(programdata_account) => {
                if let Ok(UpgradeableLoaderState::ProgramData {
                    slot: slot_on_cluster,
                    upgrade_authority_address,
                }) = programdata_account.state()
                {
                    // let data_offset = UpgradeableLoaderState::size_of_programdata_metadata();
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
                    // TODO: figure out how to correctly update the data
                    program_account.set_state(&metadata).unwrap();
                } else {
                    error!("Invalid ProgramData account");
                }
            }
            None => {
                error!("For Upgradable account the program data account needs to be provided");
            }
        }
    }
}

#[cfg(test)]
mod tests {
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

        let program_data = vec![1, 2, 3, 4, 5];
        let deployment_slot = 9999;

        let mut program_account = {
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

        let programdata_account = {
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
            &mut program_account,
            &mut Some(programdata_account),
            adjust_slot,
        );

        eprintln!("{:#?}", program_account);
    }
}
