use solana_instruction::{error::InstructionError, AccountMeta, Instruction};
use solana_pubkey::Pubkey;

const IS_SIGNER_BIT: u8 = 0;
const IS_WRITABLE_BIT: u8 = 1;

pub(crate) fn load_current_index(
    data: &[u8],
) -> Result<usize, InstructionError> {
    if data.len() < 2 {
        return Err(InstructionError::AccountDataTooSmall);
    }
    let offset = data.len() - 2;
    Ok(u16::from_le_bytes([data[offset], data[offset + 1]]) as usize)
}

pub(crate) fn load_instruction_at(
    data: &[u8],
    index: usize,
) -> Result<Instruction, InstructionError> {
    let mut cursor = 0;
    let num_instructions = read_u16(data, &mut cursor)? as usize;
    if index >= num_instructions {
        return Err(InstructionError::InvalidArgument);
    }

    cursor = 2 + index * 2;
    let start = read_u16(data, &mut cursor)? as usize;
    cursor = start;

    let num_accounts = read_u16(data, &mut cursor)? as usize;
    let mut accounts = Vec::with_capacity(num_accounts);
    for _ in 0..num_accounts {
        let flags = read_u8(data, &mut cursor)?;
        let pubkey = read_pubkey(data, &mut cursor)?;
        accounts.push(AccountMeta {
            pubkey,
            is_signer: flags & (1 << IS_SIGNER_BIT) != 0,
            is_writable: flags & (1 << IS_WRITABLE_BIT) != 0,
        });
    }

    let program_id = read_pubkey(data, &mut cursor)?;
    let data_len = read_u16(data, &mut cursor)? as usize;
    let end = cursor
        .checked_add(data_len)
        .ok_or(InstructionError::InvalidInstructionData)?;
    let instruction_data = data
        .get(cursor..end)
        .ok_or(InstructionError::InvalidInstructionData)?
        .to_vec();

    Ok(Instruction {
        program_id,
        accounts,
        data: instruction_data,
    })
}

fn read_u8(data: &[u8], cursor: &mut usize) -> Result<u8, InstructionError> {
    let value = *data
        .get(*cursor)
        .ok_or(InstructionError::InvalidInstructionData)?;
    *cursor += 1;
    Ok(value)
}

fn read_u16(data: &[u8], cursor: &mut usize) -> Result<u16, InstructionError> {
    let bytes = data
        .get(*cursor..*cursor + 2)
        .ok_or(InstructionError::InvalidInstructionData)?;
    *cursor += 2;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_pubkey(
    data: &[u8],
    cursor: &mut usize,
) -> Result<Pubkey, InstructionError> {
    let bytes = data
        .get(*cursor..*cursor + 32)
        .ok_or(InstructionError::InvalidInstructionData)?;
    *cursor += 32;
    Ok(Pubkey::new_from_array(
        bytes
            .try_into()
            .map_err(|_| InstructionError::InvalidInstructionData)?,
    ))
}
