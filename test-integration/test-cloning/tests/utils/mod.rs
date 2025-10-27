use integration_test_tools::IntegrationTestContext;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};

pub fn init_and_delegate_flexi_counter(
    ctx: &IntegrationTestContext,
    counter_auth: &Keypair,
) -> Pubkey {
    use program_flexi_counter::{instruction::*, state::*};
    ctx.airdrop_chain(&counter_auth.pubkey(), 5 * LAMPORTS_PER_SOL)
        .expect("counter auth airdrop failed");
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
