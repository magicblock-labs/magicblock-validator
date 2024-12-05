#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MagicBlockInstruction {
    ModifyAccounts,
    ScheduleCommit,
    ScheduleCommitAndUndelegate,
    ScheduledCommitSent(u64),
}

#[allow(unused)]
impl MagicBlockInstruction {
    pub(crate) fn index(&self) -> u8 {
        use MagicBlockInstruction::*;
        match self {
            ModifyAccounts => 0,
            ScheduleCommit => 1,
            ScheduleCommitAndUndelegate => 2,
            ScheduledCommitSent(_) => 3,
        }
    }

    pub(crate) fn discriminant(&self) -> [u8; 4] {
        let idx = self.index();
        [idx, 0, 0, 0]
    }
}
