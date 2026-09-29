mod account;
mod asserts;
pub use account::*;
pub use asserts::*;

#[macro_export]
macro_rules! compute {
    ($msg:expr=> $($tt:tt)*) => {
        ::solana_program::msg!(concat!($msg, " {"));
        ::solana_program::log::sol_log_compute_units();
        $($tt)*
        ::solana_program::log::sol_log_compute_units();
        ::solana_program::msg!(concat!(" } // ", $msg));
    };
}
