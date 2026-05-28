mod setup;

use magicblock_core::coordination_mode::{
    switch_to_primary_mode, switch_to_replica_mode,
};
use setup::RpcTestEnv;
use tokio::sync::{Mutex, MutexGuard};

static COORDINATION_MODE_TEST_LOCK: Mutex<()> = Mutex::const_new(());

struct ReplicaModeGuard {
    _guard: MutexGuard<'static, ()>,
}

impl ReplicaModeGuard {
    async fn enter() -> Self {
        let guard = COORDINATION_MODE_TEST_LOCK.lock().await;
        switch_to_replica_mode();
        Self { _guard: guard }
    }
}

impl Drop for ReplicaModeGuard {
    fn drop(&mut self) {
        switch_to_primary_mode();
    }
}

#[tokio::test]
async fn send_transaction_rejects_in_replica_mode() {
    let env = RpcTestEnv::new().await;
    let transfer_tx = env.build_transfer_txn();
    let _replica_mode = ReplicaModeGuard::enter().await;

    let error = env
        .rpc
        .send_transaction(&transfer_tx)
        .await
        .expect_err("sendTransaction should reject in replica mode");

    assert!(
        error.to_string().contains(
            "sendTransaction is only available while validator is primary"
        ),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn simulate_transaction_rejects_in_replica_mode() {
    let env = RpcTestEnv::new().await;
    let transfer_tx = env.build_transfer_txn();
    let _replica_mode = ReplicaModeGuard::enter().await;

    let error = env
        .rpc
        .simulate_transaction(&transfer_tx)
        .await
        .expect_err("simulateTransaction should reject in replica mode");

    assert!(
        error.to_string().contains(
            "simulateTransaction is only available while validator is primary"
        ),
        "unexpected error: {error}"
    );
}
