use solana_pubkey::Pubkey;
use solana_sdk::{instruction::Instruction, rent::Rent, signature::Keypair};
use test_kit::Signer;

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
    label: Option<String>,
) -> InitAccountAndDelegateIxs {
    const MAX_ALLOC: u64 = magicblock_committor_program::consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE as u64;

    use program_flexi_counter::{instruction::*, state::*};

    let init_counter_ix =
        create_init_ix(payer, label.unwrap_or("COUNTER".to_string()));
    let rent_exempt = Rent::default().minimum_balance(bytes as usize);

    let num_reallocs = bytes.div_ceil(MAX_ALLOC);
    let realloc_ixs = if num_reallocs == 0 {
        vec![]
    } else {
        (0..num_reallocs)
            .map(|i| create_realloc_ix(payer, bytes, i as u16))
            .collect()
    };

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

pub struct InitOrderBookAndDelegateIxs {
    pub init: Instruction,
    pub delegate: Instruction,
    pub book_manager: Keypair,
    pub order_book: Pubkey,
}

pub fn init_order_book_account_and_delegate_ixs(
    payer: Pubkey,
) -> InitOrderBookAndDelegateIxs {
    use program_schedulecommit::{api, ID};

    let book_manager = Keypair::new();

    println!("schedulecommit ID: {}", ID);

    let (order_book, _bump) = Pubkey::find_program_address(
        &[b"order_book", book_manager.pubkey().as_ref()],
        &ID,
    );

    let init_ix = api::init_order_book_instruction(
        payer,
        book_manager.pubkey(),
        order_book,
    );

    let delegate_ix = api::delegate_account_cpi_instruction(
        payer,
        None,
        book_manager.pubkey(),
        b"order_book",
    );

    InitOrderBookAndDelegateIxs {
        init: init_ix,
        delegate: delegate_ix,
        book_manager,
        order_book,
    }
}
