#![allow(unexpected_cfgs)]
use std::str::FromStr;

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey,
};

pub mod common;
pub mod instruction;
pub mod processor;
pub mod sdk;
pub mod state;

use instruction::MiniInstruction;
use processor::Processor;

static ID: Option<&str> = option_env!("MINI_PROGRAM_ID");
pub fn id() -> Pubkey {
    Pubkey::from_str(
        ID.unwrap_or("Mini111111111111111111111111111111111111111"),
    )
    .expect("Invalid program ID")
}

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let instruction = MiniInstruction::try_from(instruction_data)?;
    Processor::process(program_id, accounts, &instruction)
}

#[cfg(test)]
mod tests {
    use sdk::MiniSdk;
    use solana_program_test::*;
    use solana_sdk::{signature::Signer, transaction::Transaction};

    use super::*;

    #[tokio::test]
    async fn test_counter_init_and_increment() {
        let program_test = ProgramTest::new(
            "mini_program",
            crate::id(),
            processor!(process_instruction),
        );

        let (banks_client, payer, recent_blockhash) =
            program_test.start().await;

        let sdk = MiniSdk::new(crate::id());
        let (counter_pubkey, _) = sdk.counter_pda(&payer.pubkey());

        // Test Init instruction
        let init_ix = sdk.init_instruction(&payer.pubkey());
        let mut transaction =
            Transaction::new_with_payer(&[init_ix], Some(&payer.pubkey()));
        transaction.sign(&[&payer], recent_blockhash);

        banks_client.process_transaction(transaction).await.unwrap();

        // Verify counter is initialized to 0
        let counter_account = banks_client
            .get_account(counter_pubkey)
            .await
            .unwrap()
            .unwrap();
        let count =
            u64::from_le_bytes(counter_account.data[..8].try_into().unwrap());
        assert_eq!(count, 0);

        // Test first increment
        let increment_ix = sdk.increment_instruction(&payer.pubkey());
        let recent_blockhash =
            banks_client.get_latest_blockhash().await.unwrap();
        let mut transaction =
            Transaction::new_with_payer(&[increment_ix], Some(&payer.pubkey()));
        transaction.sign(&[&payer], recent_blockhash);

        banks_client.process_transaction(transaction).await.unwrap();

        let counter_account = banks_client
            .get_account(counter_pubkey)
            .await
            .unwrap()
            .unwrap();
        let count =
            u64::from_le_bytes(counter_account.data[..8].try_into().unwrap());
        assert_eq!(count, 1);

        // Test second increment with a different recent blockhash
        std::thread::sleep(std::time::Duration::from_millis(100));
        let increment_ix = sdk.increment_instruction(&payer.pubkey());
        let recent_blockhash =
            banks_client.get_latest_blockhash().await.unwrap();
        let mut transaction =
            Transaction::new_with_payer(&[increment_ix], Some(&payer.pubkey()));
        transaction.sign(&[&payer], recent_blockhash);

        banks_client.process_transaction(transaction).await.unwrap();

        let counter_account = banks_client
            .get_account(counter_pubkey)
            .await
            .unwrap()
            .unwrap();
        let count =
            u64::from_le_bytes(counter_account.data[..8].try_into().unwrap());
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_counter_add_shank_idl() {
        let program_test = ProgramTest::new(
            "mini_program",
            crate::id(),
            processor!(process_instruction),
        );

        let (banks_client, payer, recent_blockhash) =
            program_test.start().await;

        let sdk = MiniSdk::new(crate::id());
        let (shank_idl_pubkey, _) = sdk.shank_idl_pda();

        // Test AddShankIdl instruction
        let idl_data = b"shank_idl_data";
        let add_shank_idl_ix =
            sdk.add_shank_idl_instruction(&payer.pubkey(), idl_data);
        let mut transaction = Transaction::new_with_payer(
            &[add_shank_idl_ix],
            Some(&payer.pubkey()),
        );
        transaction.sign(&[&payer], recent_blockhash);

        banks_client.process_transaction(transaction).await.unwrap();

        // Verify Shank IDL account is created
        let shank_idl_account = banks_client
            .get_account(shank_idl_pubkey)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(shank_idl_account.data, idl_data);
    }

    #[tokio::test]
    async fn test_counter_add_anchor_idl_and_update() {
        let program_test = ProgramTest::new(
            "mini_program",
            crate::id(),
            processor!(process_instruction),
        );

        let (banks_client, payer, recent_blockhash) =
            program_test.start().await;

        let sdk = MiniSdk::new(crate::id());
        let (anchor_idl_pubkey, _) = sdk.anchor_idl_pda();

        // Test AddAnchorIdl instruction
        let idl_data = b"anchor_idl_data_v1";
        let add_anchor_idl_ix =
            sdk.add_anchor_idl_instruction(&payer.pubkey(), idl_data);
        let mut transaction = Transaction::new_with_payer(
            &[add_anchor_idl_ix],
            Some(&payer.pubkey()),
        );
        transaction.sign(&[&payer], recent_blockhash);

        banks_client.process_transaction(transaction).await.unwrap();

        // Verify Anchor IDL account is created
        let anchor_idl_account = banks_client
            .get_account(anchor_idl_pubkey)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(anchor_idl_account.data, idl_data);

        // Test updating the Anchor IDL
        let updated_idl_data = b"anchor_idl_data_v2";
        let update_anchor_idl_ix =
            sdk.add_anchor_idl_instruction(&payer.pubkey(), updated_idl_data);
        let mut transaction = Transaction::new_with_payer(
            &[update_anchor_idl_ix],
            Some(&payer.pubkey()),
        );
        transaction.sign(&[&payer], recent_blockhash);

        banks_client.process_transaction(transaction).await.unwrap();

        // Verify Anchor IDL account is updated
        let anchor_idl_account = banks_client
            .get_account(anchor_idl_pubkey)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(anchor_idl_account.data, updated_idl_data);
    }
}
