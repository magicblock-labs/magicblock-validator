use solana_sdk::{
    message,
    transaction::{SanitizedTransaction, Transaction},
};

pub fn sanitized_into_transaction(tx: SanitizedTransaction) -> Transaction {
    let message = message::legacy::Message {
        header: tx.message().header().clone(),
        account_keys: tx.message().account_keys().iter().cloned().collect(),
        recent_blockhash: tx.message().recent_blockhash().clone(),
        instructions: tx.message().instructions().to_vec(),
    };
    Transaction {
        signatures: tx.signatures().to_vec(),
        message,
    }
}
