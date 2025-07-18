use magicblock_program::magic_scheduled_l1_message::{
    CommittedAccountV2, MagicL1Message, ScheduledL1Message,
};
use solana_pubkey::Pubkey;

pub trait ScheduledMessageExt {
    fn get_committed_accounts(&self) -> Option<&Vec<CommittedAccountV2>>;
    fn get_committed_pubkeys(&self) -> Option<Vec<Pubkey>>;

    // TODO(edwin): ugly
    fn is_undelegate(&self) -> bool;
}

impl ScheduledMessageExt for ScheduledL1Message {
    fn get_committed_accounts(&self) -> Option<&Vec<CommittedAccountV2>> {
        match &self.l1_message {
            MagicL1Message::L1Actions(_) => None,
            MagicL1Message::Commit(t) => Some(t.get_committed_accounts()),
            MagicL1Message::CommitAndUndelegate(t) => {
                Some(t.get_committed_accounts())
            }
        }
    }

    fn get_committed_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.get_committed_accounts().map(|accounts| {
            accounts.iter().map(|account| account.pubkey).collect()
        })
    }

    fn is_undelegate(&self) -> bool {
        match &self.l1_message {
            MagicL1Message::L1Actions(_) => false,
            MagicL1Message::Commit(_) => false,
            MagicL1Message::CommitAndUndelegate(_) => true,
        }
    }
}
