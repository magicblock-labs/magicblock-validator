use solana_signature::Signature;
pub(super) fn short_signature(sig: &Signature) -> String {
    let sig_str = sig.to_string();
    if sig_str.len() < 8 {
        "<invalid signature>".to_string()
    } else {
        format!("{}..{}", &sig_str[..8], &sig_str[sig_str.len() - 8..])
    }
}
