#![allow(unused)]
use log::*;
use solana_sdk::hash::Hash;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::transaction::Transaction;
use std::{fmt, sync::Arc};

use solana_account::{AccountSharedData, ReadableAccount};
use solana_loader_v3_interface::{
    get_program_data_address as get_program_data_v3_address,
    state::UpgradeableLoaderState as LoaderV3State,
};
use solana_loader_v4_interface::instruction::LoaderV4Instruction as LoaderInstructionV4;
use solana_loader_v4_interface::state::{LoaderV4State, LoaderV4Status};
use solana_pubkey::Pubkey;
use solana_sdk::{pubkey, rent::Rent};
use solana_sdk_ids::bpf_loader_upgradeable;
use solana_system_interface::instruction as system_instruction;

use crate::cloner::errors::ClonerResult;
use crate::remote_account_provider::{
    ChainPubsubClient, ChainRpcClient, RemoteAccountProvider,
    RemoteAccountProviderError, RemoteAccountProviderResult,
};

// -----------------
// PDA derivation methods
// -----------------
pub fn get_loaderv3_get_program_data_address(
    program_address: &Pubkey,
) -> Pubkey {
    get_program_data_v3_address(program_address)
}

// -----------------
// LoadedProgram
// -----------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramIdl {
    pub address: Pubkey,
    pub data: Vec<u8>,
}
/// The different loader versions that exist on Solana.
/// See: docs/program-accounts.md
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteProgramLoader {
    /// Deprecated loader BPFLoader1111111111111111111111111111111111.
    /// Requires differently compiled assets to work, i.e. current SBF
    /// programs don't execute properly when loaded with this loader.
    /// Single account for both program metadata and program data.
    /// _Management instructions disabled_
    V1,
    /// Deprecated loader BPFLoader2111111111111111111111111111111111
    /// Oldest loader that accepts SBF programs and can execute them properly.
    /// All newer loaders can as well.
    /// Single account for both program metadata and program data.
    /// _Management instructions disabled_
    V2,
    /// Current loader (Aug 2025) BPFLoaderUpgradeab1e11111111111111111111111
    /// Separate accounts for program metadata and program data.
    /// _Is being phased out_
    V3,

    /// Latest loader (Aug 2025) LoaderV411111111111111111111111111111111111
    /// Not available on mainnet yet it seems, but a few programs are deployed
    /// with it on devnet.
    /// Single account for both program metadata and program data.
    /// _Expected to become the standard loader_
    V4,
}

const LOADER_V1: Pubkey =
    pubkey!("BPFLoader1111111111111111111111111111111111");
const LOADER_V2: Pubkey =
    pubkey!("BPFLoader2111111111111111111111111111111111");
pub const LOADER_V3: Pubkey =
    pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");
pub const LOADER_V4: Pubkey =
    pubkey!("LoaderV411111111111111111111111111111111111");

impl TryFrom<&Pubkey> for RemoteProgramLoader {
    type Error = RemoteAccountProviderError;

    fn try_from(loader_pubkey: &Pubkey) -> Result<Self, Self::Error> {
        use RemoteProgramLoader::*;
        match loader_pubkey {
            pubkey if pubkey.eq(&LOADER_V1) => Ok(V1),
            pubkey if pubkey.eq(&LOADER_V2) => Ok(V2),
            pubkey if pubkey.eq(&LOADER_V3) => Ok(V3),
            pubkey if pubkey.eq(&LOADER_V4) => Ok(V4),
            _ => Err(RemoteAccountProviderError::UnsupportedProgramLoader(
                loader_pubkey.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedProgram {
    pub program_id: Pubkey,
    pub authority: Pubkey,
    pub program_data: Vec<u8>,
    pub loader: RemoteProgramLoader,
    pub loader_status: LoaderV4Status,
    pub remote_slot: u64,
}

impl LoadedProgram {
    pub fn lamports(&self) -> u64 {
        let size = self.program_data.len();
        Rent::default().minimum_balance(size)
    }

    pub fn loader_id(&self) -> Pubkey {
        use RemoteProgramLoader::*;
        match self.loader {
            V1 => LOADER_V1,
            V2 => LOADER_V2,
            V3 => LOADER_V3,
            V4 => LOADER_V4,
        }
    }

    pub fn into_unsigned_deploy_transaction_v4(
        self,
        recent_blockhash: Hash,
        payer: &Pubkey,
    ) -> ClonerResult<Transaction> {
        let Self {
            program_id,
            authority,
            program_data,
            loader,
            ..
        } = self;
        let size = program_data.len() + 1024;
        let lamports = Rent::default().minimum_balance(size);

        // 1. Set program length to initialize and allocate space
        let create_program_account_instruction =
            system_instruction::create_account(
                &authority,
                &program_id,
                lamports,
                0,
                &LOADER_V4,
            );

        let set_length_instruction = {
            let loader_instruction = LoaderInstructionV4::SetProgramLength {
                new_size: size.try_into()?,
            };

            Instruction {
                program_id: LOADER_V4,
                accounts: vec![
                    // [writable] The program account to change the size of
                    AccountMeta::new(program_id, false),
                    // [signer] The authority of the program
                    AccountMeta::new_readonly(authority, true),
                ],
                data: bincode::serialize(&loader_instruction)?,
            }
        };

        // 2. Write program data in one huge chunk since the transaction is
        //    internal and has no size limit
        let write_instruction = {
            let loader_instruction = LoaderInstructionV4::Write {
                offset: 0,
                bytes: program_data.clone(),
            };

            Instruction {
                program_id: LOADER_V4,
                accounts: vec![
                    // [writable] The program account to write data to
                    AccountMeta::new(program_id, false),
                    // [signer] The authority of the program
                    AccountMeta::new_readonly(authority, true),
                ],
                data: bincode::serialize(&loader_instruction)?,
            }
        };

        // 3. Deploy the program to make it executable
        let deploy_instruction = {
            let loader_instruction = LoaderInstructionV4::Deploy;

            Instruction {
                program_id: LOADER_V4,
                accounts: vec![
                    // [writable] The program account to deploy
                    AccountMeta::new(program_id, false),
                    // [signer] The authority of the program
                    AccountMeta::new_readonly(authority, true),
                ],
                data: bincode::serialize(&loader_instruction)?,
            }
        };

        let tx = Transaction::new_with_payer(
            &[
                create_program_account_instruction,
                set_length_instruction,
                write_instruction,
                deploy_instruction,
            ],
            Some(payer),
        );
        Ok(tx)
    }
}

impl fmt::Display for LoadedProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "LoadedProgram {{
  program_id: {},
  authority: {},
  loader: {:?},
  loader_status: {:?},
  program_data: <{} bytes>
}}",
            self.program_id,
            self.authority,
            self.loader,
            self.loader_status,
            self.program_data.len()
        )
    }
}

// -----------------
// Deserialization
// -----------------
pub struct ProgramAccountResolver {
    pub program_id: Pubkey,
    pub loader: RemoteProgramLoader,
    pub authority: Pubkey,
    pub program_data: Vec<u8>,
    pub loader_status: LoaderV4Status,
    pub remote_slot: u64,
}

impl ProgramAccountResolver {
    pub fn try_new(
        program_id: Pubkey,
        owner: Pubkey,
        program_account: Option<AccountSharedData>,
        program_data_account: Option<AccountSharedData>,
    ) -> RemoteAccountProviderResult<ProgramAccountResolver> {
        let loader = RemoteProgramLoader::try_from(&owner)?;
        let (
            ProgramDataWithAuthority {
                authority,
                program_data,
                loader_status,
            },
            remote_slot,
        ) = Self::try_get_data_with_authority(
            &loader,
            &program_id,
            program_account.as_ref(),
            program_data_account.as_ref(),
        )?;
        Ok(Self {
            program_id,
            loader,
            authority,
            program_data,
            loader_status,
            remote_slot,
        })
    }

    fn try_get_data_with_authority(
        loader: &RemoteProgramLoader,
        program_id: &Pubkey,
        program_account: Option<&AccountSharedData>,
        program_data_account: Option<&AccountSharedData>,
    ) -> RemoteAccountProviderResult<(ProgramDataWithAuthority, u64)> {
        use RemoteProgramLoader::*;
        match (loader, program_account, program_data_account) {
            // Invalid cases
            (V1, None, _) => {
                Err(RemoteAccountProviderError::LoaderV1StateMissingProgramAccount(
                    *program_id,
                ))
            }
            (V2, None, _) => {
                Err(RemoteAccountProviderError::LoaderV2StateMissingProgramAccount(
                    *program_id,
                ))
            }
            (V3, _, None) => Err(
                RemoteAccountProviderError::LoaderV3StateMissingProgramDataAccount(
                    *program_id,
                ),
            ),
            (V4, None, _) => {
                Err(RemoteAccountProviderError::LoaderV4StateMissingProgramAccount(
                    *program_id,
                ))
            }
            // Valid cases
            (V1, Some(program_account), _) | (V2, Some(program_account), _) => {
                get_state_v1_v2(*program_id, program_account.data())
                    .map(|data| (data, program_account.remote_slot()))

            }
            (V3, _, Some(program_data_account)) => {
                get_state_v3(*program_id, program_data_account.data())
                    .map(|data| (data, program_data_account.remote_slot()))
            }

            (V4, Some(program_account), _) => {
                get_state_v4(*program_id, program_account.data())
                    .map(|data| (data, program_account.remote_slot()))
            }
        }
    }

    pub fn into_loaded_program(self) -> LoadedProgram {
        LoadedProgram {
            program_id: self.program_id,
            authority: self.authority,
            program_data: self.program_data,
            loader: self.loader,
            loader_status: self.loader_status,
            remote_slot: self.remote_slot,
        }
    }
}

// -----------------
// Loader State Deserialization
// -----------------
/// Unified info for deployed programs
struct ProgramDataWithAuthority {
    /// The authority that can manage the program, for loader v1-v2 this is
    /// the program ID itself.
    pub authority: Pubkey,
    /// The actual program data, i.e. the executable code which is stored in
    /// a separate account for loader v3.
    pub program_data: Vec<u8>,
    /// The loader status, only relevant for loader v4 in which case it can
    /// be [LoaderV4Status::Retracted] and in that case should not be executable
    /// in our ephemeral either after it is cloned.
    pub loader_status: LoaderV4Status,
}

fn get_state_v1_v2(
    program_id: Pubkey,
    program_account: &[u8],
) -> RemoteAccountProviderResult<ProgramDataWithAuthority> {
    debug!("Loading program account for loader v1/v2 {program_id}");
    Ok(ProgramDataWithAuthority {
        authority: program_id,
        program_data: program_account.to_vec(),
        loader_status: LoaderV4Status::Finalized,
    })
}

fn get_state_v3(
    program_id: Pubkey,
    program_data_account: &[u8],
) -> RemoteAccountProviderResult<ProgramDataWithAuthority> {
    debug!("Loading program account for loader v3 {program_id}");
    let meta_data = program_data_account
        .get(..LoaderV3State::size_of_programdata_metadata())
        .ok_or(RemoteAccountProviderError::LoaderV4StateInvalidLength(
            program_id,
            program_data_account.len(),
        ))?;
    let state =
        bincode::deserialize::<LoaderV3State>(meta_data).map_err(|err| {
            RemoteAccountProviderError::LoaderV4StateDeserializationFailed(
                program_id,
                err.to_string(),
            )
        })?;
    let program_data_with_authority = match state {
        LoaderV3State::ProgramData {
            upgrade_authority_address,
            ..
        } => {
            let authority = upgrade_authority_address
                .map(|address| Pubkey::new_from_array(address.to_bytes()))
                .unwrap_or(program_id);
            let data = program_data_account
                .get(LoaderV3State::size_of_programdata_metadata()..)
                .ok_or(
                    RemoteAccountProviderError::LoaderV4StateInvalidLength(
                        program_id,
                        program_data_account.len(),
                    ),
                )?;
            ProgramDataWithAuthority {
                authority,
                program_data: data.to_vec(),
                loader_status: LoaderV4Status::Deployed,
            }
        }
        _ => {
            return Err(RemoteAccountProviderError::UnsupportedProgramLoader(
                "LoaderV3 program data account is not in ProgramData state"
                    .to_string(),
            ))
        }
    };
    Ok(program_data_with_authority)
}

// Adapted from:
// https://github.com/anza-xyz/agave/blob/d68ec6574e80e21782e60763c114bd81e1c105b4/programs/loader-v4/src/lib.rs#L30
fn get_state_v4(
    program_id: Pubkey,
    program_account: &[u8],
) -> RemoteAccountProviderResult<ProgramDataWithAuthority> {
    debug!("Loading program account for loader v4 {program_id}");
    let data = program_account
        .get(0..LoaderV4State::program_data_offset())
        .ok_or(RemoteAccountProviderError::LoaderV4StateInvalidLength(
            program_id,
            program_account.len(),
        ))?
        .try_into()
        .unwrap();
    let state = unsafe {
        std::mem::transmute::<
            &[u8; LoaderV4State::program_data_offset()],
            &LoaderV4State,
        >(data)
    };
    let program_data = program_account
        .get(LoaderV4State::program_data_offset()..)
        .ok_or(RemoteAccountProviderError::LoaderV4StateInvalidLength(
            program_id,
            data.len(),
        ))?
        .to_vec();
    Ok(ProgramDataWithAuthority {
        authority: Pubkey::new_from_array(
            state.authority_address_or_next_version.to_bytes(),
        ),
        program_data,
        loader_status: state.status,
    })
}
