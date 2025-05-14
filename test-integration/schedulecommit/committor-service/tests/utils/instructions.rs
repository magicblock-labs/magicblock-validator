use solana_pubkey::Pubkey;
use solana_sdk::{instruction::Instruction, rent::Rent};

pub fn init_validator_fees_vault_ix(validator_auth: Pubkey) -> Instruction {
    dlp::instruction_builder::init_validator_fees_vault(
        validator_auth,
        validator_auth,
        validator_auth,
    )
}

pub struct InitAccountAndDelegateIxs {
    pub init: Instruction,
    pub reallocs: Vec<Instruction>,
    pub delegate: Instruction,
    pub pda: Pubkey,
    pub rent_excempt: u64,
}

pub fn init_account_and_delegate_ixs(
    payer: Pubkey,
    bytes: u64,
) -> InitAccountAndDelegateIxs {
    use program_flexi_counter::instruction::*;
    use program_flexi_counter::state::*;
    let init_counter_ix = create_init_ix(payer, "COUNTER".to_string());
    let rent_exempt = Rent::default().minimum_balance(bytes as usize);
    let mut realloc_ixs = vec![];
    if bytes
        > magicblock_committor_program::consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE
            as u64
    {
        // TODO: we may have to chunk those
        let reallocs = bytes
            / magicblock_committor_program::consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE
                as u64;
        for i in 0..reallocs {
            realloc_ixs.push(create_realloc_ix(payer, bytes, i as u16));
        }
    }
    let delegate_ix = create_delegate_ix(payer);
    let pda = FlexiCounter::pda(&payer).0;
    InitAccountAndDelegateIxs {
        init: init_counter_ix,
        reallocs: realloc_ixs,
        delegate: delegate_ix,
        pda,
        rent_excempt: rent_exempt,
    }
}
