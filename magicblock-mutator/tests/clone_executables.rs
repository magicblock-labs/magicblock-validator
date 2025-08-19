use assert_matches::assert_matches;
use log::*;
use magicblock_mutator::fetch::transaction_to_clone_pubkey_from_cluster;
use magicblock_program::validator;
use solana_sdk::{
    account::{Account, ReadableAccount},
    bpf_loader_upgradeable,
    clock::Slot,
    genesis_config::ClusterType,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_program,
    transaction::{SanitizedTransaction, Transaction},
};

use crate::utils::{fund_luzifer, SOLX_EXEC, SOLX_IDL, SOLX_PROG};

mod utils;

async fn verified_tx_to_clone_executable_from_devnet_first_deploy(
    pubkey: &Pubkey,
    slot: Slot,
    recent_blockhash: Hash,
) -> Transaction {
    let tx = transaction_to_clone_pubkey_from_cluster(
        &ClusterType::Devnet.into(),
        false, // We are deploying the program for the first time
        pubkey,
        recent_blockhash,
        slot,
        None,
    )
    .await
    .expect("Failed to create program clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(
        tx.signer_key(0, 0).unwrap(),
        &validator::validator_authority_id()
    );
    assert!(tx.message().account_keys.len() >= 5);
    assert!(tx.message().account_keys.len() <= 6);

    tx
}

async fn verified_tx_to_clone_executable_from_devnet_as_upgrade(
    pubkey: &Pubkey,
    slot: Slot,
    recent_blockhash: Hash,
) -> Transaction {
    let tx = transaction_to_clone_pubkey_from_cluster(
        &ClusterType::Devnet.into(),
        true, // We are upgrading the program
        pubkey,
        recent_blockhash,
        slot,
        None,
    )
    .await
    .expect("Failed to create program clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(
        tx.signer_key(0, 0).unwrap(),
        &validator::validator_authority_id()
    );
    assert!(tx.message().account_keys.len() >= 8);
    assert!(tx.message().account_keys.len() <= 9);

    tx
}

#[tokio::test]
async fn clone_executable_with_idl_and_program_data_and_then_upgrade() {
    let tx_processor = transactions_processor();
    fund_luzifer(&*tx_processor);

    tx_processor.bank().advance_slot(); // We don't want to stay on slot 0

    // 1. Exec Clone Transaction
    {
        let slot = tx_processor.bank().slot();
        let tx = verified_tx_to_clone_executable_from_devnet_first_deploy(
            &SOLX_PROG,
            slot,
            tx_processor.bank().last_blockhash(),
        )
        .await;
        let result = tx_processor.process(vec![tx]).unwrap();

        let (_, exec_details) = result.transactions.values().next().unwrap();
        log_exec_details(exec_details);
    }

    // 2. Verify that all accounts were added to the validator
    {
        let solx_prog =
            tx_processor.bank().get_account(&SOLX_PROG).unwrap().into();
        trace!("SolxProg account: {:#?}", solx_prog);

        let solx_exec =
            tx_processor.bank().get_account(&SOLX_EXEC).unwrap().into();
        trace!("SolxExec account: {:#?}", solx_exec);

        let solx_idl =
            tx_processor.bank().get_account(&SOLX_IDL).unwrap().into();
        trace!("SolxIdl account: {:#?}", solx_idl);

        assert_matches!(
            solx_prog,
            Account {
                lamports,
                data,
                owner,
                executable: true,
                rent_epoch
            } => {
                assert_eq!(lamports, 1141440);
                assert_eq!(data.len(), 36);
                assert_eq!(owner, bpf_loader_upgradeable::id());
                assert_eq!(rent_epoch, u64::MAX);
            }
        );
        assert_matches!(
            solx_exec,
            Account {
                lamports,
                data,
                owner,
                executable: false,
                rent_epoch
            } => {
                assert_eq!(lamports, 2890996080);
                assert_eq!(data.len(), 415245);
                assert_eq!(owner, bpf_loader_upgradeable::id());
                assert_eq!(rent_epoch, u64::MAX);
            }
        );
        assert_matches!(
            solx_idl,
            Account {
                lamports,
                data,
                owner,
                executable: false,
                rent_epoch
            } => {
                assert_eq!(lamports, 6264000);
                assert_eq!(data.len(), 772);
                assert_eq!(owner, elfs::solanax::id());
                assert_eq!(rent_epoch, u64::MAX);
            }
        );
    }

    // 3. Run a transaction against the cloned program
    {
        let (tx, SolanaxPostAccounts { author, post }) =
            create_solx_send_post_transaction(tx_processor.bank());
        let sig = *tx.signature();

        let result = tx_processor.process_sanitized(vec![tx]).unwrap();
        assert_eq!(result.len(), 1);

        // Transaction
        let (tx, exec_details) = result.transactions.get(&sig).unwrap();

        log_exec_details(exec_details);
        assert!(exec_details.status.is_ok());
        assert_eq!(tx.signatures().len(), 2);
        assert_eq!(tx.message().account_keys().len(), 4);

        // Signature Status
        let sig_status = tx_processor.bank().get_signature_status(&sig);
        assert!(sig_status.is_some());
        assert_matches!(sig_status.as_ref().unwrap(), Ok(()));

        // Accounts checks
        let author_acc = tx_processor.bank().get_account(&author).unwrap();
        assert_eq!(author_acc.data().len(), 0);
        assert_eq!(author_acc.owner(), &system_program::ID);
        assert_eq!(author_acc.lamports(), LAMPORTS_PER_SOL);

        let post_acc = tx_processor.bank().get_account(&post).unwrap();
        assert_eq!(post_acc.data().len(), 1180);
        assert_eq!(post_acc.owner(), &elfs::solanax::ID);
        assert_eq!(post_acc.lamports(), 9103680);
    }

    // 4. Exec Upgrade Transactions
    {
        let slot = tx_processor.bank().slot();
        let tx = verified_tx_to_clone_executable_from_devnet_as_upgrade(
            &SOLX_PROG,
            slot,
            tx_processor.bank().last_blockhash(),
        )
        .await;
        let result = tx_processor.process(vec![tx]).unwrap();

        let (_, exec_details) = result.transactions.values().next().unwrap();
        log_exec_details(exec_details);
    }

    // 5. Run a transaction against the upgraded program
    {
        // For an upgraded program: `effective_slot = deployed_slot + 1`
        // Therefore to activate it we need to advance a slot
        tx_processor.bank().advance_slot();

        let (tx, SolanaxPostAccounts { author, post }) =
            create_solx_send_post_transaction(tx_processor.bank());
        let sig = *tx.signature();

        let result = tx_processor.process_sanitized(vec![tx]).unwrap();
        assert_eq!(result.len(), 1);

        // Transaction
        let (tx, exec_details) = result.transactions.get(&sig).unwrap();

        log_exec_details(exec_details);
        assert!(exec_details.status.is_ok());
        assert_eq!(tx.signatures().len(), 2);
        assert_eq!(tx.message().account_keys().len(), 4);

        // Signature Status
        let sig_status = tx_processor.bank().get_signature_status(&sig);
        assert!(sig_status.is_some());
        assert_matches!(sig_status.as_ref().unwrap(), Ok(()));

        // Accounts checks
        let author_acc = tx_processor.bank().get_account(&author).unwrap();
        assert_eq!(author_acc.data().len(), 0);
        assert_eq!(author_acc.owner(), &system_program::ID);
        assert_eq!(author_acc.lamports(), LAMPORTS_PER_SOL - 2);

        let post_acc = tx_processor.bank().get_account(&post).unwrap();
        assert_eq!(post_acc.data().len(), 1180);
        assert_eq!(post_acc.owner(), &elfs::solanax::ID);
        assert_eq!(post_acc.lamports(), 9103680);
    }
}

// SolanaX
pub struct SolanaxPostAccounts {
    pub post: Pubkey,
    pub author: Pubkey,
}
pub fn create_solx_send_post_transaction(
    bank: &Bank,
) -> (SanitizedTransaction, SolanaxPostAccounts) {
    let accounts = vec![
        create_funded_account(
            bank,
            Some(Rent::default().minimum_balance(1180)),
        ),
        create_funded_account(bank, Some(LAMPORTS_PER_SOL)),
    ];
    let post = &accounts[0];
    let author = &accounts[1];
    let instruction =
        create_solx_send_post_instruction(&elfs::solanax::id(), &accounts);
    let message = Message::new(&[instruction], Some(&author.pubkey()));
    let transaction =
        Transaction::new(&[author, post], message, bank.last_blockhash());
    (
        SanitizedTransaction::try_from_legacy_transaction(
            transaction,
            &Default::default(),
        )
        .unwrap(),
        SolanaxPostAccounts {
            post: post.pubkey(),
            author: author.pubkey(),
        },
    )
}

fn create_solx_send_post_instruction(
    program_id: &Pubkey,
    funded_accounts: &[Keypair],
) -> Instruction {
    // https://explorer.solana.com/tx/nM2WLNPVfU3R8C4dJwhzwBsVXXgBkySAuBrGTEoaGaAQMxNHy4mnAgLER8ddDmD6tjw3suVhfG1RdbdbhyScwLK?cluster=devnet
    #[rustfmt::skip]
    let ix_bytes: Vec<u8> = vec![
        0x84, 0xf5, 0xee, 0x1d,
        0xf3, 0x2a, 0xad, 0x36,
        0x05, 0x00, 0x00, 0x00,
        0x68, 0x65, 0x6c, 0x6c,
        0x6f,
    ];
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![
            AccountMeta::new(funded_accounts[0].pubkey(), true),
            AccountMeta::new(funded_accounts[1].pubkey(), true),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
    )
}
