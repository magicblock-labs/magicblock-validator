use integration_test_tools::IntegrationTestContext;
use log::*;
use solana_sdk::{
    account::Account, instruction::Instruction, loader_v4,
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer, transaction::Transaction,
};
use test_kit::init_logger;

#[test]
fn test_clone_old_bpf_and_run_transaction() {
    // NOTE: this is actually not testing a _really_ old BPF program.
    // I left it here anyways, more program types are tested in ./03_program-deploy.rs
    init_logger!();
    const MEMO_PROGRAM_PK: Pubkey = Pubkey::new_from_array([
        5, 74, 83, 90, 153, 41, 33, 6, 77, 36, 232, 113, 96, 218, 56, 124, 124,
        53, 181, 221, 188, 146, 187, 129, 228, 31, 168, 64, 65, 5, 68, 141,
    ]);
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = Keypair::new();
    ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL)
        .expect("failed to airdrop to on-chain account");

    let memo_ix = Instruction::new_with_bytes(
        MEMO_PROGRAM_PK,
        &[
            0x39, 0x34, 0x32, 0x32, 0x38, 0x30, 0x37, 0x2e, 0x35, 0x34, 0x30,
            0x30, 0x30, 0x32,
        ],
        vec![],
    );
    let tx = Transaction::new_signed_with_payer(
        &[memo_ix],
        Some(&payer.pubkey()),
        &[&payer],
        ctx.ephem_blockhash.unwrap(),
    );
    let signature = ctx
        .try_ephem_client()
        .unwrap()
        .send_and_confirm_transaction_with_spinner(&tx)
        .unwrap();
    debug!("MEMO program cloning success: {}", signature);
    let account = ctx
        .try_ephem_client()
        .unwrap()
        .get_account(&MEMO_PROGRAM_PK)
        .unwrap();
    let Account {
        owner, executable, ..
    } = account;
    assert_eq!(owner, loader_v4::id());
    assert!(executable);
}
