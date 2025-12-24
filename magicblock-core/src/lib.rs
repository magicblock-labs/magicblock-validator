pub type Slot = u64;

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

pub mod compression;
pub mod link;
pub mod tls;
pub mod traits;
