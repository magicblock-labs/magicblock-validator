//! Ephemeral account instruction processors
//!
//! Ephemeral accounts are zero-balance accounts with rent paid by a sponsor.
//! Rent is charged at 32 lamports/byte (109x cheaper than Solana base rent).

mod process_close;
mod process_create;
mod process_resize;
mod processor;
mod validation;

pub(crate) use process_close::process_close_ephemeral_account;
pub(crate) use process_create::process_create_ephemeral_account;
pub(crate) use process_resize::process_resize_ephemeral_account;
