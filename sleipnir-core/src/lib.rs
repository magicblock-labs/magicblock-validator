pub mod magic_program {
    use solana_sdk::pubkey;
    pub use solana_sdk::pubkey::Pubkey;

    pub const MAGIC_PROGRAM_ADDR: &str =
        "Magic11111111111111111111111111111111111111";
    solana_sdk::declare_id!("Magic11111111111111111111111111111111111111");

    pub const MAGIC_CONTEXT_PUBKEY: Pubkey =
        pubkey!("MagicContext1111111111111111111111111111111");

    pub const MAGIC_CONTEXT_SIZE: usize = 1024 * 1024 * 100; // 100 MB
}

/// A macro that panics when running a debug build and logs the panic message
/// instead when running in release mode.
#[macro_export]
macro_rules! debug_panic {
    ($($arg:tt)*) => (
        if cfg!(debug_assertions) {
            panic!($($arg)*);
        } else {
            ::log::error!($($arg)*);
        }
    )
}
