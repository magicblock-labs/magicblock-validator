use log::*;
use sleipnir_bank::bank_dev_utils::elfs;
use sleipnir_bank::bank_dev_utils::transactions::create_solx_send_post_transaction;
use solana_sdk::account::{Account, ReadableAccount};
use solana_sdk::bpf_loader_upgradeable;
use test_tools::{init_logger, transactions_processor};

use assert_matches::assert_matches;
use test_tools::account::get_account_addr;
use test_tools::diagnostics::log_exec_details;

use crate::utils::{
    fund_luzifer, verified_tx_to_clone_from_devnet, SOLX_EXEC, SOLX_IDL, SOLX_PROG,
};

mod utils;

#[tokio::test]
async fn clone_solx_executable() {
    init_logger!();

    let tx_processor = transactions_processor();
    fund_luzifer(&*tx_processor);

    // 1. Exec Clone Transaction

    {
        let slot = tx_processor.bank().slot();
        let tx = verified_tx_to_clone_from_devnet(SOLX_PROG, slot, 5).await;
        let result = tx_processor.process(vec![tx]).unwrap();

        let (_, exec_details) = result.transactions.values().next().unwrap();
        log_exec_details(exec_details);
    }

    // 2. Verify that all accounts were added to the validator
    {
        let solx_prog: Account = get_account_addr(tx_processor.bank(), SOLX_PROG)
            .unwrap()
            .into();
        trace!("SolxProg account: {:#?}", solx_prog);

        let solx_exec: Account = get_account_addr(tx_processor.bank(), SOLX_EXEC)
            .unwrap()
            .into();
        trace!("SolxExec account: {:#?}", solx_exec);

        let solx_idl: Account = get_account_addr(tx_processor.bank(), SOLX_IDL)
            .unwrap()
            .into();
        trace!("SolxIdl account: {:#?}", solx_idl);

        assert_matches!(
            solx_prog,
            Account {
                lamports: l,
                data: d,
                owner: o,
                executable: true,
                rent_epoch: r
            } => {
                assert!(l >= 1141440);
                assert!(d.len() >= 36);
                assert_eq!(o, bpf_loader_upgradeable::id());
                assert_eq!(r, u64::MAX);
            }
        );
        assert_matches!(
            solx_exec,
            Account {
                lamports: l,
                data: d,
                owner: o,
                executable: false,
                rent_epoch: r
            } => {
                assert!(l >= 2890996080);
                assert!(d.len() >= 415245);
                assert_eq!(o, bpf_loader_upgradeable::id());
                assert_eq!(r, u64::MAX);
            }
        );
        assert_matches!(
            solx_idl,
            Account {
                lamports: l,
                data: d,
                owner: o,
                executable: false,
                rent_epoch: r
            } => {
                assert!(l >= 6264000);
                assert!(d.len() >= 772);
                assert_eq!(o, elfs::solanax::id());
                assert_eq!(r, u64::MAX);
            }
        );
    }

    // 3. Run a transaction against the cloned program
    {
        // For a deployed program: `effective_slot = deployed_slot + 1`
        // Therefore to activate it we need to advance a slot
        tx_processor.bank().advance_slot();

        // TODO: I'm not sure why payer/post seem backwards here
        // However the lamports don't make sesne even then as the post account was
        // debited  and the payer account did not change lamports at all
        let (tx, post, payer) = create_solx_send_post_transaction(tx_processor.bank());
        {
            let payer_acc = tx_processor.bank().get_account(&payer).unwrap();
            let post_acc = tx_processor.bank().get_account(&post).unwrap();
            debug!("Payer account: {:#?}", payer_acc);
            debug!("Post account: {:#?}", post_acc);
        }

        let result = tx_processor.process_sanitized(vec![tx]).unwrap();
        assert_eq!(result.len(), 1);
        for (sig, (tx, exec_details)) in result.transactions {
            log_exec_details(&exec_details);
            assert!(exec_details.status.is_ok());
            assert_eq!(tx.signatures().len(), 2);
            assert_eq!(tx.message().account_keys().len(), 4);

            let sig_status = tx_processor.bank().get_signature_status(&sig);
            assert!(sig_status.is_some());
            assert_matches!(sig_status.as_ref().unwrap(), Ok(()));
        }
        {
            let payer_acc = tx_processor.bank().get_account(&payer).unwrap();
            let post_acc = tx_processor.bank().get_account(&post).unwrap();
            assert_eq!(post_acc.data().len(), 1180);
            debug!("Payer account: {:#?}", payer_acc);
            debug!("Post account: {:#?}", post_acc);
            // 1000000000
            //  999990000
        }
    }
}
