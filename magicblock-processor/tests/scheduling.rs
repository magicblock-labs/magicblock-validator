use std::{collections::HashSet, time::Duration};

use guinea::GuineaInstruction;
use magicblock_core::{
    link::transactions::TransactionResult, traits::AccountsBank,
};
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use test_kit::{ExecutionTestEnv, Signer};
