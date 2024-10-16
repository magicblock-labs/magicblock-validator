pub enum AccountClone<'a> {
    Wallet { pubkey: &'a str },
    Undelegated { pubkey: &'a str, owner: &'a str },
    Delegated { pubkey: &'a str, owner: &'a str },
    Program { pubkey: &'a str },
}

pub enum AccountCommit<'a> {
    CommitOnly { pubkey: &'a str },
    CommitAndUndelegate { pubkey: &'a str },
}
