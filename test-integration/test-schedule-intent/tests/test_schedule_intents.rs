use dlp::pda::ephemeral_balance_pda_from_payer;
use integration_test_tools::{expect, IntegrationTestContext};
use program_flexi_counter::delegation_program_id;
use program_flexi_counter::instruction::{
    create_add_ix, create_delegate_ix, create_init_ix, create_intent_ix,
};
use program_flexi_counter::state::FlexiCounter;
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::rent::Rent;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

const SLOT_MS: u64 = 150;

#[test]
fn test_schedule_intent() {
    const LABEL: &str = "I am label";

    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = setup_payer(&ctx);

    // Init counter
    {
        let ix = create_init_ix(payer.pubkey(), LABEL.to_string());
        let (_, confirmed) = ctx.send_and_confirm_instructions_with_payer_chain(&[ix], &payer)
            .unwrap();
        assert!(confirmed, "Should confirm transaction");

        let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
        let counter = ctx.fetch_chain_account_struct::<FlexiCounter>(counter_pda).unwrap();
        assert_eq!(
            counter,
            FlexiCounter {
                count: 0,
                updates: 0,
                label: LABEL.to_string()
            },
        )
    }

    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
    // Delegate counter
    {
        ctx.wait_for_next_slot_ephem().unwrap();
        let ix = create_delegate_ix(payer.pubkey());
        ctx.send_and_confirm_instructions_with_payer_chain(&[ix], &payer)
            .unwrap();

        // Confirm delegated
        let owner = ctx.fetch_chain_account_owner(counter_pda).unwrap();
        assert_eq!(owner, delegation_program_id());
    }

    // Set counter to 101
    {
        ctx.wait_for_next_slot_ephem().unwrap();
        let ix = create_add_ix(payer.pubkey(), 101);
        ctx.send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
            .unwrap();

        let counter = ctx
            .fetch_ephem_account_struct::<FlexiCounter>(counter_pda)
            .unwrap();
        assert_eq!(
            counter,
            FlexiCounter {
                count: 101,
                updates: 1,
                label: LABEL.to_string()
            },
        )
    }

    // Schedule Intent
    {
        ctx.wait_for_next_slot_ephem().unwrap();

        let transfer_destination = Keypair::new();
        let ix = create_intent_ix(payer.pubkey(), transfer_destination.pubkey(), 0, false, 100_000);

        let (sig, confirmed) = ctx.send_and_confirm_instructions_with_payer_ephem(&[ix], &payer).unwrap();
        assert!(confirmed);

        // Confirm was sent on Base Layer
        let commit_result = ctx
            .fetch_schedule_commit_result::<FlexiCounter>(sig)
            .unwrap();
        commit_result
            .confirm_commit_transactions_on_chain(&ctx)
            .unwrap();

        // Confirm results on base lauer
        let counter = ctx
            .fetch_chain_account_struct::<FlexiCounter>(counter_pda)
            .unwrap();
        assert_eq!(
            counter,
            FlexiCounter {
                count: 101,
                updates: 1,
                label: LABEL.to_string()
            },
        );

        // ensure Prize = 10_000 is transferred
        let transfer_destination_balance = ctx.fetch_chain_account_balance(&transfer_destination.pubkey()).unwrap();
        assert_eq!(transfer_destination_balance, 1_000_000);
    }
}

fn setup_payer(ctx: &IntegrationTestContext) -> Keypair {
    let payer = Keypair::new();
    ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL)
        .unwrap();

    // Create actor escrow
    let ix = dlp::instruction_builder::top_up_ephemeral_balance(
        payer.pubkey(),
        payer.pubkey(),
        Some(LAMPORTS_PER_SOL / 2),
        Some(1),
    );
    ctx.send_and_confirm_instructions_with_payer_chain(&[ix], &payer)
        .unwrap();

    // Confirm actor escrow
    let escrow_pda = ephemeral_balance_pda_from_payer(&payer.pubkey(), 1);
    let rent = Rent::default().minimum_balance(0);
    println!("rent: {}", rent);
    assert_eq!(
        ctx.fetch_chain_account(escrow_pda).unwrap().lamports,
        LAMPORTS_PER_SOL / 2 + rent
    );

    payer
}
