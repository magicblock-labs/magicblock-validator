use solana_pubkey::Pubkey;

#[allow(unused)]
pub fn stringify_maybe_pubkeys(pubkeys: &[Option<Pubkey>]) -> Vec<String> {
    pubkeys
        .iter()
        .map(|pk_opt| match pk_opt {
            Some(pk) => pk.to_string(),
            None => "<None>".to_string(),
        })
        .collect()
}

#[allow(unused)]
pub fn stringify_pubkeys(pubkeys: &[Pubkey]) -> Vec<String> {
    pubkeys.iter().map(|pk| pk.to_string()).collect()
}
