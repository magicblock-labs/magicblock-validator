use nucleus::runtime::TransactionView;
use solana_system_interface::instruction::SystemInstruction;

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
/// The only variable overhead is wincode deserialization of the instruction
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
pub(crate) fn is_noop_system_transfer(tx: &TransactionView) -> bool {
    // Early exit: Must have exactly 1 instruction
    let mut instructions = tx.program_instructions_iter();
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
    // Performance: wincode deserialization is O(1) for fixed instruction size
    let Ok(SystemInstruction::Transfer { lamports }) =
        wincode::deserialize::<SystemInstruction>(instruction.data)
    else {
        return false;
    };

    lamports == 0
}

#[cfg(test)]
mod tests {
    use nucleus::testkit::signed_view;
    use solana_hash::Hash;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_keypair::Keypair;
    use solana_pubkey::Pubkey;
    use solana_signer::Signer;
    use solana_system_interface::instruction as system_instruction;

    use super::*;

    #[test]
    fn test_zero_lamports_transfer() {
        let payer = Keypair::new();
        let transfer_ix = system_instruction::transfer(
            &payer.pubkey(),
            &Pubkey::new_unique(),
            0,
        );
        let (_, sanitized_tx) =
            signed_view(&payer, &[transfer_ix], Hash::default());

        assert!(is_noop_system_transfer(&sanitized_tx));
    }

    #[test]
    fn test_nonzero_lamports_transfer() {
        let payer = Keypair::new();
        let transfer_ix = system_instruction::transfer(
            &payer.pubkey(),
            &Pubkey::new_unique(),
            1000,
        );
        let (_, sanitized_tx) =
            signed_view(&payer, &[transfer_ix], Hash::default());

        assert!(!is_noop_system_transfer(&sanitized_tx));
    }

    #[test]
    fn test_multiple_instructions() {
        let payer = Keypair::new();
        let transfer_ix =
            system_instruction::transfer(&payer.pubkey(), &payer.pubkey(), 0);
        let allocate_ix = system_instruction::allocate(&payer.pubkey(), 1024);
        let (_, sanitized_tx) =
            signed_view(&payer, &[transfer_ix, allocate_ix], Hash::default());

        assert!(!is_noop_system_transfer(&sanitized_tx));
    }

    #[test]
    fn test_no_instructions() {
        let payer = Keypair::new();
        let (_, sanitized_tx) = signed_view(&payer, &[], Hash::default());

        assert!(!is_noop_system_transfer(&sanitized_tx));
    }

    #[test]
    fn test_non_system_instruction() {
        let payer = Keypair::new();
        let non_system_ix = Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new(Pubkey::new_unique(), false),
            ],
            data: vec![],
        };
        let (_, sanitized_tx) =
            signed_view(&payer, &[non_system_ix], Hash::default());

        assert!(!is_noop_system_transfer(&sanitized_tx));
    }
}
