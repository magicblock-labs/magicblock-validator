use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

// NOTE: we don't really verify anything here, but need this for the health check
// to work unchanged.
// We may remove this later if it's not needed.
#[derive(Debug, Default)]
pub struct VerifyAccountsHashInBackground {
    /// true when verification has completed or never had to run in background
    pub verified: Arc<AtomicBool>,
}

impl VerifyAccountsHashInBackground {
    /// notify that verification was completed successfully
    /// This can occur because it completed in the background
    /// or if the verification was run in the foreground.
    pub fn verification_complete(&self) {
        self.verified.store(true, Ordering::Release);
    }
}
