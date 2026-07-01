use std::time::Duration;

use integration_test_tools::{expect, validator::cleanup};
use magicblock_task_scheduler::{crank_pubkey, db::DbTask, SchedulerDatabase};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
};
use test_task_scheduler::{
    setup_validator_with_migration_tasks, wait_for_empty_db,
    wait_for_hydra_crank,
};
use tokio::runtime::Runtime;

/// Tasks persisted by the legacy (validator-funded) scheduler are migrated onto
/// hydra at startup: a funded hydra crank is created for each, and the
/// migration database is emptied. The database is used only for this one-time
/// migration.
#[test]
fn test_migration_reschedules_tasks_and_empties_db() {
    let authority = Keypair::new().pubkey();
    let task_id = 42;
    let iterations = 2;
    // A simple, signer-free instruction is enough for the crank to be created;
    // migration does not execute it.
    let instructions = vec![Instruction {
        program_id: program_flexi_counter::ID,
        accounts: vec![AccountMeta::new(Pubkey::new_unique(), false)],
        data: vec![0],
    }];
    let task = DbTask {
        id: task_id,
        authority,
        execution_interval_millis: 100,
        executions_left: iterations,
        instructions,
    };

    let (temp_dir, mut validator, ctx, _) =
        setup_validator_with_migration_tasks(&[task]);

    // Migration creates a funded hydra crank for the persisted task...
    let crank_pda = crank_pubkey(&authority, task_id);
    wait_for_hydra_crank(
        &ctx,
        &crank_pda,
        Duration::from_secs(15),
        &mut validator,
    );

    // ...and empties the migration database.
    let db = expect!(
        SchedulerDatabase::new(SchedulerDatabase::path(temp_dir.path())),
        validator
    );
    let runtime = expect!(Runtime::new(), validator);
    wait_for_empty_db(&runtime, &db, Duration::from_secs(15), &mut validator);

    cleanup(&mut validator);
}
