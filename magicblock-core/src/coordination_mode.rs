use std::sync::atomic::{AtomicU8, Ordering};

/// Global coordination mode accessible from any crate.
///
/// Stored as an atomic u8 for lock-free reads from program
/// instruction handlers.
///
/// Transitions:
///   StartingUp (0) → Primary (1)   [Standalone validators]
///   StartingUp (0) → Replica (2)   [StandBy/ReplicatOnly validators]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CoordinationMode {
    /// Ledger replay phase — no validator signer, no side effects.
    StartingUp = 0,
    /// Primary mode — requires validator signer, allows side effects.
    Primary = 1,
    /// Replica mode — no validator signer, no side effects.
    Replica = 2,
}

static COORDINATION_MODE: AtomicU8 = if cfg!(test) {
    AtomicU8::new(CoordinationMode::Primary as u8)
} else {
    AtomicU8::new(CoordinationMode::StartingUp as u8)
};

impl CoordinationMode {
    fn from_u8(val: u8) -> Self {
        match val {
            0 => Self::StartingUp,
            1 => Self::Primary,
            2 => Self::Replica,
            _ => panic!("Invalid coordination mode value: {val}"),
        }
    }

    /// Returns the current global coordination mode.
    pub fn current() -> Self {
        Self::from_u8(COORDINATION_MODE.load(Ordering::Acquire))
    }

    /// Whether the validator signer is required for transactions.
    pub fn needs_validator_signer(self) -> bool {
        matches!(self, Self::Primary)
    }

    /// Whether intents (scheduled tasks, commits) should be executed.
    pub fn should_schedule_intents(self) -> bool {
        matches!(self, Self::Primary)
    }
}

/// Whether the validator signer is required for transactions.
pub fn needs_validator_signer() -> bool {
    CoordinationMode::current().needs_validator_signer()
}

/// Whether intents (scheduled tasks, commits) should be executed.
pub fn should_schedule_intents() -> bool {
    CoordinationMode::current().should_schedule_intents()
}

/// Transitions from `StartingUp` to `Primary`.
/// No-op if already in Primary mode (happens in tests that start in Primary).
pub fn switch_to_primary_mode() {
    let target = CoordinationMode::Primary as u8;
    let mut current = COORDINATION_MODE.load(Ordering::Acquire);
    loop {
        if current == target {
            return;
        }
        match COORDINATION_MODE.compare_exchange(
            CoordinationMode::StartingUp as u8,
            target,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(actual) => {
                if actual == target {
                    return;
                }
                assert_eq!(
                    actual,
                    CoordinationMode::StartingUp as u8,
                    "switch_to_primary_mode: expected StartingUp, got {:?}",
                    CoordinationMode::from_u8(actual)
                );
                current = actual;
            }
        }
    }
}

/// Resets the process-global coordination mode back to `StartingUp`
/// so tests do not leak state between each other.
#[cfg(test)]
pub(crate) fn reset_coordination_mode_for_tests() {
    COORDINATION_MODE
        .store(CoordinationMode::StartingUp as u8, Ordering::Release);
}

/// Transitions from `StartingUp` to `Replica`.
/// No-op if already in Replica mode.
pub fn switch_to_replica_mode() {
    let target = CoordinationMode::Replica as u8;
    let mut current = COORDINATION_MODE.load(Ordering::Acquire);
    loop {
        if current == target {
            return;
        }
        match COORDINATION_MODE.compare_exchange(
            CoordinationMode::StartingUp as u8,
            target,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(actual) => {
                if actual == target {
                    return;
                }
                assert_eq!(
                    actual,
                    CoordinationMode::StartingUp as u8,
                    "switch_to_replica_mode: expected StartingUp, got {:?}",
                    CoordinationMode::from_u8(actual)
                );
                current = actual;
            }
        }
    }
}
