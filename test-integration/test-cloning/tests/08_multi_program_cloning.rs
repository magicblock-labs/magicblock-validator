use integration_test_tools::IntegrationTestContext;
use log::*;
use program_mini::sdk::MiniSdk;
use solana_sdk::{
    instruction::Instruction, native_token::LAMPORTS_PER_SOL,
    signature::Keypair, signer::Signer,
};
use test_chainlink::programs::{PARALLEL_MINIV3_1, PARALLEL_MINIV3_2};
use test_kit::init_logger;

/// This test verifies that we can clone two LoaderV3 programs together by
/// sending a transaction that invokes both programs. Both programs are
/// cloned from the local devnet, demonstrating that the batched fetching
/// optimization works correctly when multiple LoaderV3 programs need to be
/// fetched and cloned in a single batch.
#[test]
fn test_clone_two_programs_in_single_transaction() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    // Create SDK instances for both parallel programs
    let sdk_prog1 = MiniSdk::new(PARALLEL_MINIV3_1);
    let sdk_prog2 = MiniSdk::new(PARALLEL_MINIV3_2);

    let payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&payer, 2 * LAMPORTS_PER_SOL)
        .unwrap();

    // Create instructions for both programs
    let msg_prog1 = "Hello from Parallel Program 1";
    let msg_prog2 = "Hello from Parallel Program 2";

    let ix_prog1: Instruction =
        sdk_prog1.log_msg_instruction(&payer.pubkey(), msg_prog1);
    let ix_prog2: Instruction =
        sdk_prog2.log_msg_instruction(&payer.pubkey(), msg_prog2);

    // Send a transaction that invokes both programs.
    // This exercises the batched fetching optimization since both programs
    // are LoaderV3 programs that need their program data accounts fetched.
    debug!(
        "Sending transaction with instructions for both parallel programs..."
    );
    let (sig, found) = ctx
        .send_and_confirm_instructions_with_payer_ephem(
            &[ix_prog1, ix_prog2],
            &payer,
        )
        .unwrap();

    debug!(
        "Transaction sent with signature {}. Found on chain: {}",
        sig, found
    );
    assert!(found, "Transaction was not found on chain");

    // Verify both programs executed correctly by checking logs
    if let Some(logs) = ctx.fetch_ephemeral_logs(sig) {
        debug!("Transaction logs: {:?}", logs);
        assert!(
            logs.contains(&format!("Program log: LogMsg: {}", msg_prog1)),
            "First parallel program instruction did not execute correctly"
        );
        assert!(
            logs.contains(&format!("Program log: LogMsg: {}", msg_prog2)),
            "Second parallel program instruction did not execute correctly"
        );
    } else {
        panic!("No logs found for transaction {}", sig);
    }

    debug!("Test passed: Successfully cloned and executed two LoaderV3 programs in a single transaction");
}
