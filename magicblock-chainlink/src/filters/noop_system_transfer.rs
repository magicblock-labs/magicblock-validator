use solana_system_interface::instruction::SystemInstruction;
use solana_transaction::sanitized::SanitizedTransaction;

/// Detects transactions that are no-op system transfers (lamports == 0).
///
/// This function identifies transactions created by users that perform a
/// system transfer with zero lamports, which is a no-op that wastes transaction
/// fees without any meaningful state change.
///
/// # Performance Notes
///
/// This function is O(1) in terms of iterations:
/// - Early return after checking instruction count (constant time)
/// - Single iteration through at most 1 instruction (bounded to 1)
/// - System instruction deserialization is O(1) for fixed-size data
///
/// The only variable overhead is bincode deserialization of the instruction
/// data (~8-100 bytes), which is negligible compared to transaction processing.
///
/// # Arguments
///
/// * `tx` - The sanitized transaction to check
///
/// # Returns
///
/// `true` if the transaction is a single system transfer instruction where
/// the transferred lamports are zero, `false` otherwise.
pub(crate) fn is_noop_system_transfer(tx: &SanitizedTransaction) -> bool {
    let message = tx.message();

    // Early exit: Must have exactly 1 instruction
    let mut instructions = message.program_instructions_iter();
    let Some(first) = instructions.next() else {
        return false;
    };
    if instructions.next().is_some() {
        return false;
    }

    let (program_id, instruction) = first;

    // Check if this is the system program instruction
    // Performance: This is a single Pubkey comparison (32 bytes)
    if program_id != &solana_system_interface::program::ID {
        return false;
    }

    // Attempt to parse the instruction data as a system instruction
    // Performance: bincode deserialization is O(1) for fixed instruction size
    let Ok(SystemInstruction::Transfer { lamports }) =
        bincode::deserialize::<SystemInstruction>(&instruction.data)
    else {
        return false;
    };

    lamports == 0
}

#[cfg(test)]
mod tests {
    use solana_instruction::{AccountMeta, Instruction};
    use solana_keypair::Keypair;
    use solana_message::Message;
    use solana_pubkey::Pubkey;
    use solana_signer::Signer;
    use solana_system_interface::instruction as system_instruction;
    use solana_transaction::Transaction;

    use super::*;

    #[test]
    fn test_zero_lamports_transfer() {
        let payer = Keypair::new();
        let from = Keypair::new();
        let to = Keypair::new();

        let transfer_ix =
            system_instruction::transfer(&from.pubkey(), &to.pubkey(), 0);

        let message = Message::new(&[transfer_ix], Some(&payer.pubkey()));
        let tx = Transaction::new_unsigned(message);
        let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(tx);

        assert!(is_noop_system_transfer(&sanitized_tx));
    }

    #[test]
    fn test_nonzero_lamports_transfer() {
        let payer = Keypair::new();
        let from = Keypair::new();
        let to = Keypair::new();

        let transfer_ix =
            system_instruction::transfer(&from.pubkey(), &to.pubkey(), 1000);

        let message = Message::new(&[transfer_ix], Some(&payer.pubkey()));
        let tx = Transaction::new_unsigned(message);
        let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(tx);

        assert!(!is_noop_system_transfer(&sanitized_tx));
    }

    #[test]
    fn test_multiple_instructions() {
        let payer = Keypair::new();
        let account = Keypair::new();

        let transfer_ix = system_instruction::transfer(
            &account.pubkey(),
            &account.pubkey(),
            0,
        );
        let allocate_ix = system_instruction::allocate(&account.pubkey(), 1024);

        let message =
            Message::new(&[transfer_ix, allocate_ix], Some(&payer.pubkey()));
        let tx = Transaction::new_unsigned(message);
        let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(tx);

        assert!(!is_noop_system_transfer(&sanitized_tx));
    }

    #[test]
    fn test_no_instructions() {
        let payer = Keypair::new();
        let message = Message::new(&[], Some(&payer.pubkey()));
        let tx = Transaction::new_unsigned(message);
        let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(tx);

        assert!(!is_noop_system_transfer(&sanitized_tx));
    }

    #[test]
    fn test_non_system_instruction() {
        let payer = Keypair::new();
        let account = Keypair::new();

        let non_system_ix = Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![
                AccountMeta::new(account.pubkey(), false),
                AccountMeta::new(account.pubkey(), false),
            ],
            data: vec![],
        };

        let message = Message::new(&[non_system_ix], Some(&payer.pubkey()));
        let tx = Transaction::new_unsigned(message);
        let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(tx);

        assert!(!is_noop_system_transfer(&sanitized_tx));
    }
}