use integration_test_tools::Signer;
use solana_pubkey::Pubkey;
use solana_sdk::{instruction::Instruction, rent::Rent, signature::Keypair};

pub fn init_validator_fees_vault_ix(validator_auth: Pubkey) -> Instruction {
    dlp_api::instruction_builder::init_validator_fees_vault(
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
    _label: Option<String>,
) -> InitAccountAndDelegateIxs {
    use program_schedulecommit::api::{
        delegate_account_cpi_instruction, init_order_book_instruction,
        UserSeeds,
    };

    let pda = account_pda(&payer);
    let init_counter_ix = init_order_book_instruction(payer, payer, pda);
    let rent_exempt = Rent::default().minimum_balance(bytes as usize);
    let realloc_ixs = Vec::new();
    let delegate_ix = delegate_account_cpi_instruction(
        payer,
        None,
        payer,
        UserSeeds::OrderBook,
    );
    InitAccountAndDelegateIxs {
        init: init_counter_ix,
        reallocs: realloc_ixs,
        delegate: delegate_ix,
        pda,
        rent_excempt: rent_exempt,
    }
}

pub fn account_pda(authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"order_book", authority.as_ref()],
        &program_schedulecommit::ID,
    )
    .0
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
        api::UserSeeds::OrderBook,
    );

    InitOrderBookAndDelegateIxs {
        init: init_ix,
        delegate: delegate_ix,
        book_manager,
        order_book,
    }
}
