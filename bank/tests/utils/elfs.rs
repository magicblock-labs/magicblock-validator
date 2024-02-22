use solana_sdk::{
    account::{Account, AccountSharedData},
    bpf_loader_upgradeable::UpgradeableLoaderState,
    pubkey::Pubkey,
    rent::Rent,
};

pub mod noop {
    solana_sdk::declare_id!("nooPu5P1NcgyXypBLNiH6VWBet5XtpPMKjCCN6CbDpW");
}

pub mod solanax {
    solana_sdk::declare_id!("SoLXmnP9JvL6vJ7TN1VqtTxqsc2izmPfF9CsMDEuRzJ");
}

static ELFS: &[(Pubkey, Pubkey, &[u8])] = &[
    (
        noop::ID,
        solana_sdk::bpf_loader_upgradeable::ID,
        // solana_sdk::bpf_loader::ID,
        include_bytes!("elfs/noop.so"),
    ),
    (
        solanax::ID,
        solana_sdk::bpf_loader_upgradeable::ID,
        include_bytes!("elfs/solanax.so"),
    ),
];

pub fn elf_accounts() -> Vec<(Pubkey, AccountSharedData)> {
    let rent = Rent::default();
    ELFS.iter()
        .flat_map(|(program_id, loader_id, elf)| {
            let mut accounts = vec![];
            let data = if *loader_id == solana_sdk::bpf_loader_upgradeable::ID {
                let (programdata_address, _) =
                    Pubkey::find_program_address(&[program_id.as_ref()], loader_id);
                let mut program_data = bincode::serialize(&UpgradeableLoaderState::ProgramData {
                    slot: 0,
                    upgrade_authority_address: Some(Pubkey::default()),
                })
                .unwrap();
                program_data.extend_from_slice(elf);
                accounts.push((
                    programdata_address,
                    AccountSharedData::from(Account {
                        lamports: rent.minimum_balance(program_data.len()).max(1),
                        data: program_data,
                        owner: *loader_id,
                        executable: false,
                        rent_epoch: 0,
                    }),
                ));
                bincode::serialize(&UpgradeableLoaderState::Program {
                    programdata_address,
                })
                .unwrap()
            } else {
                elf.to_vec()
            };
            accounts.push((
                *program_id,
                AccountSharedData::from(Account {
                    lamports: rent.minimum_balance(data.len()).max(1),
                    data,
                    owner: *loader_id,
                    executable: true,
                    rent_epoch: 0,
                }),
            ));
            accounts.into_iter()
        })
        .collect()
}

pub fn elf_accounts_for(program_id: &Pubkey) -> Vec<(Pubkey, AccountSharedData)> {
    let program = elf_accounts()
        .into_iter()
        .find(|(id, _)| id == program_id)
        .expect("elf program not found");
    let (programdata_address, _) = Pubkey::find_program_address(
        &[program_id.as_ref()],
        &solana_sdk::bpf_loader_upgradeable::ID,
    );
    let programdata = elf_accounts()
        .into_iter()
        .find(|(id, _)| id == &programdata_address)
        .expect("elf programdata not found");

    vec![program, programdata]
}
