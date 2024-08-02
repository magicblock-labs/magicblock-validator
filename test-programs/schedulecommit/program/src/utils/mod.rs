use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    msg,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};

// -----------------
// Asserts
// -----------------
pub fn assert_keys_equal<F: FnOnce() -> String>(
    provided_key: &Pubkey,
    expected_key: &Pubkey,
    get_msg: F,
) -> ProgramResult {
    if provided_key.ne(expected_key) {
        msg!("Err: {}", get_msg());
        msg!("Err: provided {} expected {}", provided_key, expected_key);
        Err(ProgramError::Custom(1))
    } else {
        Ok(())
    }
}

pub fn assert_is_signer(
    account: &AccountInfo,
    account_label: &str,
) -> ProgramResult {
    if !account.is_signer {
        msg!(
            "Err: account '{}' ({}) should be signer",
            account_label,
            account.key
        );
        Err(ProgramError::MissingRequiredSignature)
    } else {
        Ok(())
    }
}

// -----------------
// Account Operations
// -----------------
pub struct AllocateAndAssignAccountArgs<'a, 'b> {
    pub payer_info: &'a AccountInfo<'a>,
    pub account_info: &'a AccountInfo<'a>,
    pub owner: &'a Pubkey,
    pub size: usize,
    pub signer_seeds: &'b [&'b [u8]],
}

#[inline(always)]
pub fn allocate_account_and_assign_owner(
    args: AllocateAndAssignAccountArgs,
) -> Result<(), ProgramError> {
    let rent = Rent::get()?;
    let AllocateAndAssignAccountArgs {
        payer_info,
        account_info,
        owner,
        size,
        signer_seeds,
    } = args;

    let required_lamports = rent
        .minimum_balance(size)
        .max(1)
        .saturating_sub(account_info.lamports());

    // 1. Transfer the required rent to the account
    if required_lamports > 0 {
        transfer_lamports(payer_info, account_info, required_lamports)?;
    }

    // 2. Allocate the space to hold data
    //    At this point the account is still owned by the system program
    msg!("  create_account() allocate space");
    invoke_signed(
        &system_instruction::allocate(
            account_info.key,
            size.try_into().unwrap(),
        ),
        // 0. `[WRITE, SIGNER]` New account
        &[account_info.clone()],
        &[signer_seeds],
    )?;

    // 3. Assign the owner of the account so that it can sign on its behalf
    msg!("  create_account() assign to owning program");
    invoke_signed(
        &system_instruction::assign(account_info.key, owner),
        // 0. `[WRITE, SIGNER]` Assigned account public key
        &[account_info.clone()],
        &[signer_seeds],
    )?;

    Ok(())
}

#[inline(always)]
pub fn transfer_lamports<'a>(
    payer_info: &AccountInfo<'a>,
    to_account_info: &AccountInfo<'a>,
    lamports: u64,
) -> Result<(), ProgramError> {
    msg!("  transfer_lamports()");
    if payer_info.lamports() < lamports {
        msg!("Err: payer has only {} lamports", payer_info.lamports());
        return Err(ProgramError::InsufficientFunds);
    }
    invoke(
        &system_instruction::transfer(
            payer_info.key,
            to_account_info.key,
            lamports,
        ),
        &[payer_info.clone(), to_account_info.clone()],
    )
}
