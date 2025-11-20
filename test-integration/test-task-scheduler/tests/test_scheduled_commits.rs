use cleanass::assert;
use integration_test_tools::{expect, validator::cleanup};
use program_flexi_counter::{
    instruction::create_schedule_task_ix, state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use test_task_scheduler::{
    create_delegated_counter, send_noop_tx, setup_validator,
};

#[test]
fn test_scheduled_commits() {
    let (_temp_dir, mut validator, ctx) = setup_validator();

    let payer = Keypair::new();
    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    // Noop tx to make sure the noop program is cloned
    let ephem_blockhash = send_noop_tx(&ctx, &payer, &mut validator);

    let commit_frequency_ms = 400;
    create_delegated_counter(&ctx, &payer, &mut validator, commit_frequency_ms);

    eprintln!("Delegated counter: {:?}", counter_pda);

    // Schedule a task
    let task_id = 5;
    // Interval matching mainnet block time
    let execution_interval_millis = 400;
    let iterations = 3;
    let sig = expect!(
        ctx.send_transaction_ephem_with_preflight(
            &mut Transaction::new_signed_with_payer(
                &[create_schedule_task_ix(
                    payer.pubkey(),
                    task_id,
                    execution_interval_millis,
                    iterations,
                    false,
                    false,
                )],
                Some(&payer.pubkey()),
                &[&payer],
                ephem_blockhash,
            ),
            &[&payer]
        ),
        validator
    );
    let status = expect!(ctx.get_transaction_ephem(&sig), validator);
    expect!(
        status
            .transaction
            .meta
            .and_then(|m| m.status.ok())
            .ok_or_else(|| anyhow::anyhow!("Transaction failed")),
        validator
    );

    // Check that the counter value is properly incremented on mainnet
    const MAX_TRIES: u32 = 30;
    let mut tries = 0;
    let mut chain_values = vec![0, 1, 2];
    let mut ephem_values = vec![0, 1, 2];
    loop {
        let chain_counter_account = expect!(
            ctx.try_chain_client().and_then(|client| client
                .get_account(&counter_pda)
                .map_err(|e| anyhow::anyhow!("Failed to get account: {}", e))),
            validator
        );
        let chain_counter = expect!(
            FlexiCounter::try_decode(&chain_counter_account.data),
            validator
        );

        let ephem_counter_account = expect!(
            ctx.try_ephem_client().and_then(|client| client
                .get_account(&counter_pda)
                .map_err(|e| anyhow::anyhow!("Failed to get account: {}", e))),
            validator
        );
        let ephem_counter = expect!(
            FlexiCounter::try_decode(&ephem_counter_account.data),
            validator
        );

        let chain_value_index =
            chain_values.iter().position(|x| x == &chain_counter.count);
        if let Some(index) = chain_value_index {
            chain_values.remove(index);
        }

        let ephem_value_index =
            ephem_values.iter().position(|x| x == &ephem_counter.count);
        if let Some(index) = ephem_value_index {
            ephem_values.remove(index);
        }

        if chain_values.is_empty() && ephem_values.is_empty() {
            break;
        }

        tries += 1;
        if tries > MAX_TRIES {
            assert!(
                false,
                cleanup(&mut validator),
                "Missed some values: ephem_values: {:?}, chain_values: {:?}",
                ephem_values,
                chain_values
            );
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    cleanup(&mut validator);
}
