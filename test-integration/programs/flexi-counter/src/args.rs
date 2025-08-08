use borsh::{BorshDeserialize, BorshSerialize};

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct CommitActionData {
    pub transfer_amount: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct UndelegateActionData {
    pub counter_diff: i64,
    pub transfer_amount: u64,
}
