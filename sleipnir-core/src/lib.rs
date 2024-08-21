pub mod magic_program {
    use solana_program::pubkey;
    use solana_sdk::pubkey::Pubkey;

    pub const MAGIC_PROGRAM_ADDR: Pubkey =
        pubkey!("Magic11111111111111111111111111111111111111");

    solana_sdk::declare_id!("Magic11111111111111111111111111111111111111");
}
