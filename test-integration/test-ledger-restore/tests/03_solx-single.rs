use integration_test_tools::expect;
use integration_test_tools::tmpdir::resolve_tmp_dir;
use sleipnir_config::ProgramConfig;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signature::Signature;
use solana_sdk::signer::Signer;
use solana_sdk::system_program;
use solana_sdk::transaction::Transaction;
use std::path::Path;
use std::process::Child;
use test_ledger_restore::{
    setup_offline_validator, SLOT_WRITE_DELTA, SOLX_ID, SOLX_PUBKEY,
    TMP_DIR_LEDGER,
};

#[test]
fn restore_ledger_with_solx_post() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let sender = Keypair::new();
    let post = Keypair::new();

    let (mut validator, sig, slot) = write(&ledger_path, &sender, &post);
    validator.kill().unwrap();

    let mut validator =
        read(&ledger_path, &sender.pubkey(), &post.pubkey(), slot);
    validator.kill().unwrap();
}

fn get_programs() -> Vec<ProgramConfig> {
    vec![ProgramConfig {
        id: SOLX_ID.try_into().unwrap(),
        path: "fixtures/elfs/solanax.so".to_string(),
    }]
}

fn write(
    ledger_path: &Path,
    sender: &Keypair,
    post: &Keypair,
) -> (Child, Signature, u64) {
    let programs = get_programs();
    let (_, mut validator, ctx) =
        setup_offline_validator(ledger_path, Some(programs), true);

    // 1. Airdrop to sender
    expect!(
        ctx.airdrop_ephem(&sender.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    // 2. Create post transaction
    let send_post_ix_args = [
        0x84, 0xf5, 0xee, 0x1d, 0xf3, 0x2a, 0xad, 0x36, 0x0b, 0x00, 0x00, 0x00,
        0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x77, 0x6f, 0x72, 0x6c, 0x64,
    ];
    let ix = Instruction::new_with_bytes(
        SOLX_PUBKEY,
        &send_post_ix_args,
        vec![
            AccountMeta::new(sender.pubkey(), true),
            AccountMeta::new(post.pubkey(), true),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
    );
    let mut tx = Transaction::new_with_payer(&[ix], Some(&sender.pubkey()));
    let signers = &[post, sender];
    let (sig, _confirmed) = expect!(
        ctx.send_and_confirm_transaction_ephem(&mut tx, signers),
        validator
    );

    let slot = ctx.wait_for_delta_slot_ephem(SLOT_WRITE_DELTA).unwrap();

    (validator, sig, slot)
}

fn read(
    ledger_path: &Path,
    sender: &Pubkey,
    post: &Pubkey,
    slot: u64,
) -> Child {
    let (_, mut validator, ctx) =
        setup_offline_validator(ledger_path, None, false);

    assert!(ctx.wait_for_slot_ephem(slot).is_ok());

    let sender_acc = expect!(ctx.ephem_client.get_account(sender), validator);
    let post_acc = expect!(ctx.ephem_client.get_account(post), validator);

    eprintln!("Sender: {:?}", sender_acc);
    eprintln!("Post: {:#?}", post_acc);

    validator
}
