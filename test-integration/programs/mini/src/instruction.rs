use solana_program::program_error::ProgramError;

#[derive(Debug, Clone, PartialEq)]
pub enum MiniInstruction {
    /// Initialize the counter account
    ///
    /// Accounts:
    /// 0. `[signer, writable]` Payer account
    /// 1. `[writable]` Counter PDA account
    /// 2. `[]` System program
    Init,
    /// Increment the counter by 1
    ///
    /// Accounts:
    /// 0. `[signer]` Payer account
    /// 1. `[writable]` Counter PDA account
    Increment,

    /// Accounts:
    /// 0. `[signer]` Payer account
    /// 1. `[writable]` Shank IDL PDA account
    /// 2. `[]` System program
    AddShankIdl(Vec<u8>),

    /// Accounts:
    /// 0. `[signer]` Payer account
    /// 1. `[writable]` Anchor IDL PDA account
    /// 2. `[]` System program
    AddAnchorIdl(Vec<u8>),

    /// 0. `[signer]` Payer account
    LogMsg(String),
}

impl TryFrom<&[u8]> for MiniInstruction {
    type Error = ProgramError;

    fn try_from(data: &[u8]) -> Result<Self, Self::Error> {
        if data.is_empty() {
            return Err(ProgramError::InvalidInstructionData);
        }

        match data[0] {
            0 => Ok(MiniInstruction::Init),
            1 => Ok(MiniInstruction::Increment),
            2 => Ok(MiniInstruction::AddShankIdl(data[1..].to_vec())),
            3 => Ok(MiniInstruction::AddAnchorIdl(data[1..].to_vec())),
            4 => Ok(MiniInstruction::LogMsg(
                String::from_utf8(data[1..].to_vec())
                    .map_err(|_| ProgramError::InvalidInstructionData)?,
            )),
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

impl From<MiniInstruction> for Vec<u8> {
    fn from(instruction: MiniInstruction) -> Self {
        match instruction {
            MiniInstruction::Init => vec![0],
            MiniInstruction::Increment => vec![1],
            MiniInstruction::AddShankIdl(idl) => {
                vec![2].into_iter().chain(idl).collect()
            }
            MiniInstruction::AddAnchorIdl(idl) => {
                vec![3].into_iter().chain(idl).collect()
            }
            MiniInstruction::LogMsg(msg) => {
                vec![4].into_iter().chain(msg.into_bytes()).collect()
            }
        }
    }
}
