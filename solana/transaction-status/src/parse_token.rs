use serde::{Deserialize, Serialize};
use std::str::FromStr;

// NOTE: from account-decoder/src/parse_token.rs
pub type StringAmount = String;
pub type StringDecimals = String;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UiTokenAmount {
    pub ui_amount: Option<f64>,
    pub decimals: u8,
    pub amount: StringAmount,
    pub ui_amount_string: StringDecimals,
}

impl UiTokenAmount {
    pub fn real_number_string(&self) -> String {
        real_number_string(
            u64::from_str(&self.amount).unwrap_or_default(),
            self.decimals,
        )
    }

    pub fn real_number_string_trimmed(&self) -> String {
        if !self.ui_amount_string.is_empty() {
            self.ui_amount_string.clone()
        } else {
            real_number_string_trimmed(
                u64::from_str(&self.amount).unwrap_or_default(),
                self.decimals,
            )
        }
    }
}

pub fn real_number_string(amount: u64, decimals: u8) -> StringDecimals {
    let decimals = decimals as usize;
    if decimals > 0 {
        // Left-pad zeros to decimals + 1, so we at least have an integer zero
        let mut s = format!("{:01$}", amount, decimals + 1);
        // Add the decimal point (Sorry, "," locales!)
        s.insert(s.len() - decimals, '.');
        s
    } else {
        amount.to_string()
    }
}

pub fn real_number_string_trimmed(amount: u64, decimals: u8) -> StringDecimals {
    let mut s = real_number_string(amount, decimals);
    if decimals > 0 {
        let zeros_trimmed = s.trim_end_matches('0');
        s = zeros_trimmed.trim_end_matches('.').to_string();
    }
    s
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransactionTokenBalance {
    pub account_index: u8,
    pub mint: String,
    pub ui_token_amount: UiTokenAmount,
    pub owner: String,
    pub program_id: String,
}
