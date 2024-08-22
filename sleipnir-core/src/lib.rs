pub mod magic_program {
    pub const MAGIC_PROGRAM_ADDR: &str =
        "Magic11111111111111111111111111111111111111";
    solana_sdk::declare_id!("Magic11111111111111111111111111111111111111");
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
