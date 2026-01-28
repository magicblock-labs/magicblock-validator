// use magicblock_core::link::transactions::{
//     ProcessableTransaction, SanitizeableTransaction, TransactionProcessingMode,
// };
// use solana_keypair::Keypair;
// use solana_program::{
//     hash::Hash,
//     instruction::{AccountMeta, Instruction},
// };
// use solana_pubkey::Pubkey;
// use solana_signer::Signer;
// use solana_transaction::Transaction;

// use super::coordinator::ExecutionCoordinator;

// /// Creates a mock transaction with the specified accounts: `[(Pubkey, is_writable)]`
// fn mock_txn(accounts: &[(Pubkey, bool)]) -> ProcessableTransaction {
//     let payer = Keypair::new();
//     let instructions: Vec<Instruction> = accounts
//         .iter()
//         .map(|(pubkey, is_writable)| {
//             let meta = if *is_writable {
//                 AccountMeta::new(*pubkey, false)
//             } else {
//                 AccountMeta::new_readonly(*pubkey, false)
//             };
//             Instruction::new_with_bincode(Pubkey::new_unique(), &(), vec![meta])
//         })
//         .collect();

//     let transaction = Transaction::new_signed_with_payer(
//         &instructions,
//         Some(&payer.pubkey()),
//         &[payer],
//         Hash::new_unique(),
//     );

//     ProcessableTransaction {
//         transaction: transaction.sanitize(false).unwrap(),
//         mode: TransactionProcessingMode::Execution(None),
//     }
// }

// #[test]
// fn test_non_conflicting_transactions() {
//     let mut c = ExecutionCoordinator::new(2);
//     let txn1 = mock_txn(&[(Pubkey::new_unique(), true)]);
//     let txn2 = mock_txn(&[(Pubkey::new_unique(), true)]);

//     let e1 = c.get_ready_executor().unwrap();
//     let e2 = c.get_ready_executor().unwrap();

//     assert!(c.try_acquire_locks(e1, &txn1).is_ok());
//     assert!(c.try_acquire_locks(e2, &txn2).is_ok());
// }

// #[test]
// fn test_read_read_no_contention() {
//     let mut c = ExecutionCoordinator::new(2);
//     let acc = Pubkey::new_unique();

//     let txn1 = mock_txn(&[(acc, false)]);
//     let txn2 = mock_txn(&[(acc, false)]);

//     let e1 = c.get_ready_executor().unwrap();
//     let e2 = c.get_ready_executor().unwrap();

//     assert!(c.try_acquire_locks(e1, &txn1).is_ok());
//     assert!(c.try_acquire_locks(e2, &txn2).is_ok());
// }

// #[test]
// fn test_write_write_contention() {
//     let mut c = ExecutionCoordinator::new(2);
//     let acc = Pubkey::new_unique();

//     let txn1 = mock_txn(&[(acc, true)]);
//     let txn2 = mock_txn(&[(acc, true)]);

//     let e1 = c.get_ready_executor().unwrap();
//     let e2 = c.get_ready_executor().unwrap();

//     assert!(c.try_acquire_locks(e1, &txn1).is_ok());
//     assert_eq!(c.try_acquire_locks(e2, &txn2).unwrap_err(), e1);
// }

// #[test]
// fn test_write_read_contention() {
//     let mut c = ExecutionCoordinator::new(2);
//     let acc = Pubkey::new_unique();

//     let txn1 = mock_txn(&[(acc, true)]);
//     let txn2 = mock_txn(&[(acc, false)]);

//     let e1 = c.get_ready_executor().unwrap();
//     let e2 = c.get_ready_executor().unwrap();

//     assert!(c.try_acquire_locks(e1, &txn1).is_ok());
//     assert_eq!(c.try_acquire_locks(e2, &txn2).unwrap_err(), e1);
// }

// #[test]
// fn test_read_write_contention() {
//     let mut c = ExecutionCoordinator::new(2);
//     let acc = Pubkey::new_unique();

//     let txn1 = mock_txn(&[(acc, false)]);
//     let txn2 = mock_txn(&[(acc, true)]);

//     let e1 = c.get_ready_executor().unwrap();
//     let e2 = c.get_ready_executor().unwrap();

//     assert!(c.try_acquire_locks(e1, &txn1).is_ok());
//     assert_eq!(c.try_acquire_locks(e2, &txn2).unwrap_err(), e1);
// }

// #[test]
// fn test_try_schedule_success() {
//     let mut c = ExecutionCoordinator::new(2);
//     let txn = mock_txn(&[(Pubkey::new_unique(), true)]);
//     let e = c.get_ready_executor().unwrap();

//     assert!(c.try_schedule(e, txn).is_ok());
// }

// #[test]
// fn test_try_schedule_blocked_and_queued() {
//     let mut c = ExecutionCoordinator::new(2);
//     let acc = Pubkey::new_unique();

//     let txn1 = mock_txn(&[(acc, true)]);
//     let txn2 = mock_txn(&[(acc, true)]);

//     let e1 = c.get_ready_executor().unwrap();
//     let e2 = c.get_ready_executor().unwrap();

//     assert!(c.try_schedule(e1, txn1).is_ok());
//     assert_eq!(c.try_schedule(e2, txn2).err(), Some(e1));

//     // e2 should be released back to the pool
//     assert!(c.get_ready_executor().is_some());

//     // txn2 should be in e1's blocked queue
//     assert!(c.next_blocked_transaction(e1).is_some());
// }

// #[test]
// fn test_unlock_and_reschedule() {
//     let mut c = ExecutionCoordinator::new(2);
//     let acc = Pubkey::new_unique();

//     let txn1 = mock_txn(&[(acc, true)]);
//     let txn2 = mock_txn(&[(acc, true)]);

//     let e1 = c.get_ready_executor().unwrap();
//     let e2 = c.get_ready_executor().unwrap();

//     assert!(c.try_schedule(e1, txn1).is_ok());
//     assert_eq!(c.try_schedule(e2, txn2).err(), Some(e1));

//     // Simulate e1 finishing: unlock and get blocked txn
//     c.unlock_accounts(e1);
//     let blocked = c.next_blocked_transaction(e1).unwrap();

//     // Now txn2 should be able to acquire the lock
//     assert!(c.try_acquire_locks(e1, &blocked).is_ok());
// }

// #[test]
// fn test_multiple_reads_concurrent() {
//     let mut c = ExecutionCoordinator::new(3);
//     let acc = Pubkey::new_unique();

//     let txn1 = mock_txn(&[(acc, false)]);
//     let txn2 = mock_txn(&[(acc, false)]);
//     let txn3 = mock_txn(&[(acc, false)]);

//     let e1 = c.get_ready_executor().unwrap();
//     let e2 = c.get_ready_executor().unwrap();
//     let e3 = c.get_ready_executor().unwrap();

//     assert!(c.try_acquire_locks(e1, &txn1).is_ok());
//     assert!(c.try_acquire_locks(e2, &txn2).is_ok());
//     assert!(c.try_acquire_locks(e3, &txn3).is_ok());
// }

// #[test]
// fn test_empty_transaction() {
//     let mut c = ExecutionCoordinator::new(1);
//     let txn = mock_txn(&[]);
//     let e = c.get_ready_executor().unwrap();
//     assert!(c.try_acquire_locks(e, &txn).is_ok());
// }

// #[test]
// fn test_release_and_reacquire() {
//     let mut c = ExecutionCoordinator::new(1);
//     let a = Pubkey::new_unique();
//     let b = Pubkey::new_unique();

//     let txn1 = mock_txn(&[(a, true)]);
//     let txn2 = mock_txn(&[(b, true)]);

//     let e = c.get_ready_executor().unwrap();
//     assert!(c.try_acquire_locks(e, &txn1).is_ok());
//     c.unlock_accounts(e);
//     assert!(c.try_acquire_locks(e, &txn2).is_ok());
// }
