#[derive(Debug, PartialEq, Eq)]
pub enum LifecycleMode {
    Replica,
    ProgramsReplica,
    Ephemeral,
    Offline,
}
