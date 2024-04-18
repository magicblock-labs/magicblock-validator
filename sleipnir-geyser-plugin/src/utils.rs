use geyser_grpc_proto::geyser::SubscribeUpdateTransaction;
use solana_sdk::signature::Signature;

pub fn short_signature_from_sub_update(
    tx: &SubscribeUpdateTransaction,
) -> String {
    tx.transaction
        .as_ref()
        .map(|x| short_signature_from_vec(&x.signature))
        .unwrap_or("<missing transaction>".to_string())
}

pub fn short_signature_from_vec(sig: &[u8]) -> String {
    match Signature::try_from(sig) {
        Ok(sig) => short_signature(&sig),
        Err(_) => "<invalid signature>".to_string(),
    }
}

pub fn short_signature(sig: &Signature) -> String {
    let sig_str = sig.to_string();
    if sig_str.len() < 8 {
        "<invalid signature>".to_string()
    } else {
        format!("{}..{}", &sig_str[..8], &sig_str[sig_str.len() - 8..])
    }
}
