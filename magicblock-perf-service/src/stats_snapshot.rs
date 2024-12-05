use magicblock_bank::bank::Bank;

pub(crate) struct StatsSnapshot {
    pub num_transactions: u64,
    pub num_non_vote_transactions: u64,
    pub highest_slot: u64,
}

impl StatsSnapshot {
    pub(crate) fn from_bank(bank: &Bank) -> Self {
        Self {
            num_transactions: bank.transaction_count(),
            num_non_vote_transactions: bank
                .non_vote_transaction_count_since_restart(),
            highest_slot: bank.slot(),
        }
    }

    pub(crate) fn diff_since(&self, predecessor: &Self) -> (u64, u64, u64) {
        (
            self.num_transactions
                .saturating_sub(predecessor.num_transactions),
            self.num_non_vote_transactions
                .saturating_sub(predecessor.num_non_vote_transactions),
            self.highest_slot.saturating_sub(predecessor.highest_slot),
        )
    }
}
