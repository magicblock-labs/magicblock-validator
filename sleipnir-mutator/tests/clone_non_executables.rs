use sleipnir_bank::bank_dev_utils::init_logger;
use sleipnir_mutator::Mutator;
use sleipnir_program::sleipnir_authority_id;
use solana_sdk::{genesis_config::ClusterType, hash::Hash};

const SOLX_TIPS: &str = "SoLXtipsYqzgFguFCX6vw3JCtMChxmMacWdTpz2noRX";

#[tokio::test]
async fn clone_non_executable_without_data() {
    init_logger();
    let mutator = Mutator::default();

    let recent_blockhash = Hash::default();
    let tx = mutator
        .transaction_to_clone_account_from_cluster(ClusterType::Devnet, SOLX_TIPS, recent_blockhash)
        .await
        .expect("Failed to create clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(tx.signer_key(0, 0).unwrap(), &sleipnir_authority_id());
    assert_eq!(tx.message().account_keys.len(), 3);
}
