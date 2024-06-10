use std::error::Error;

use sleipnir_bank::{
    bank::Bank,
    program_loader::{add_loadables, LoadableProgram},
};
use solana_sdk::{
    bpf_loader_upgradeable::{self},
    pubkey::Pubkey,
};

// -----------------
// Methods to add programs to the bank
// -----------------
/// Uses the default loader to load programs which need to be provided in
/// a single string as follows:
///
/// ```text
/// "<program_id>:<full_path>,<program_id>:<full_path>,..."
/// ```
pub fn load_programs_from_string_config(
    bank: &Bank,
    programs: &str,
) -> Result<(), Box<dyn Error>> {
    fn extract_program_info_from_parts(
        s: &str,
    ) -> Result<LoadableProgram, Box<dyn Error>> {
        let parts = s.trim().split(':').collect::<Vec<_>>();
        if parts.len() != 2 {
            return Err(format!("Invalid program definition: {}", s).into());
        }
        let program_id = parts[0].parse::<Pubkey>()?;
        let full_path = parts[1].to_string();
        Ok(LoadableProgram::new(
            program_id,
            bpf_loader_upgradeable::ID,
            full_path,
        ))
    }

    let loadables = programs
        .split(',')
        .collect::<Vec<_>>()
        .into_iter()
        .map(extract_program_info_from_parts)
        .collect::<Result<Vec<LoadableProgram>, Box<dyn Error>>>()?;

    add_loadables(bank, &loadables)?;

    Ok(())
}
