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

pub enum CallHandlerDiscriminator {
    Simple = 0,
    // On post-undelegation we delegate account back
    ReDelegate = 1,
}

impl CallHandlerDiscriminator {
    pub fn to_array(&self) -> [u8; 4] {
        match self {
            Self::Simple => [0, 0, 0, 0],
            Self::ReDelegate => [0, 0, 0, 1],
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.to_array().to_vec()
    }
}
