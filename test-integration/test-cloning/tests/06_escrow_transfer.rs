use integration_test_tools::IntegrationTestContext;
use log::*;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer, system_instruction,
};
use test_kit::init_logger;

fn init_and_delegate_flexi_counter(
    ctx: &IntegrationTestContext,
    counter_auth: &Keypair,
) -> Pubkey {
    use program_flexi_counter::{instruction::*, state::*};
    let init_counter_ix =
        create_init_ix(counter_auth.pubkey(), "COUNTER".to_string());
    let delegate_ix = create_delegate_ix(counter_auth.pubkey());
    ctx.send_and_confirm_instructions_with_payer_chain(
        &[init_counter_ix, delegate_ix],
        counter_auth,
    )
    .unwrap();
    FlexiCounter::pda(&counter_auth.pubkey()).0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_transfer_from_escrow_to_delegated_account() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    // 1. Create account with 2 SOL + escrow 1 SOL and a counter account
    let kp_escrow = Keypair::new();
    let kp_counter = Keypair::new();

    let (
        _airdrop_sig,
        _escrow_sig,
        ephemeral_balance_pda_1,
        _deleg_record,
        escrow_lamports_1,
    ) = ctx
        .airdrop_chain_escrowed(&kp_escrow, 2 * LAMPORTS_PER_SOL)
        .await
        .unwrap();
    let counter_pda = init_and_delegate_flexi_counter(&ctx, &kp_counter);

    assert_eq!(
        ctx.fetch_ephem_account(ephemeral_balance_pda_1)
            .unwrap()
            .lamports,
        escrow_lamports_1
    );

    debug!("{:#?}", ctx.fetch_ephem_account(counter_pda).unwrap());

    // 2. Transfer 0.5 SOL from kp1 to counter pda
    let transfer_amount = 5 * LAMPORTS_PER_SOL / 2;
    let transfer_ix = system_instruction::transfer(
        &kp_escrow.pubkey(),
        &counter_pda,
        transfer_amount,
    );
    let (sig, confirmed) = ctx
        .send_and_confirm_instructions_with_payer_ephem(
            &[transfer_ix],
            &kp_escrow,
        )
        .unwrap();

    debug!("Transfer tx: {sig} {confirmed}");

    // 3. Check balances
    let acc1 = ctx.fetch_ephem_account(ephemeral_balance_pda_1);
    let acc2 = ctx.fetch_ephem_account(counter_pda);
    debug!("Account: {acc1:#?}");
    debug!("Counter: {acc2:#?}");
}
