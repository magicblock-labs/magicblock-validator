use std::fmt;

use num_derive::FromPrimitive;
use solana_sdk::{
    decode_error::DecodeError,
    instruction::InstructionError,
    msg,
    program_error::{PrintProgramError, ProgramError},
};
use thiserror::Error;

#[derive(Debug)]
pub struct MagicErrorWithContext {
    pub error: MagicError,
    pub context: String,
}

impl MagicErrorWithContext {
    pub fn new(error: MagicError, context: String) -> Self {
        Self { error, context }
    }
}

impl fmt::Display for MagicErrorWithContext {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} ({:?})", self.context, self.error)
    }
}

#[derive(Clone, Debug, Eq, Error, FromPrimitive, PartialEq)]
pub enum MagicError {
    #[error("An internal error occurred.")]
    InternalError = 0x888,
    #[error("The account is not delegated to the ephemeral validator.")]
    AccountNotDelegated,
    #[error("The number of accounts provided is larger than expected.")]
    TooManyAccountsProvided,
    #[error("The program was provided as the payer account.")]
    ProgramCannotBePayer,
}

impl PrintProgramError for MagicError {
    fn print<E>(&self) {
        msg!(&self.to_string());
    }
}

impl From<MagicError> for ProgramError {
    fn from(e: MagicError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

impl From<MagicError> for InstructionError {
    fn from(e: MagicError) -> Self {
        InstructionError::Custom(e as u32)
    }
}

impl<T> DecodeError<T> for MagicError {
    fn type_of() -> &'static str {
        "Magic Error"
    }
}
