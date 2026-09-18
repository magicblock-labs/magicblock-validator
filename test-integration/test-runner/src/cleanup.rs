use std::{
    process::{self, Child},
    time::Duration,
};

use integration_test_tools::validator::stop_validator;

pub fn cleanup_validators(
    ephem_validator: &mut Child,
    devnet_validator: &mut Child,
) {
    cleanup_validator(ephem_validator, "ephemeral");
    cleanup_validator(devnet_validator, "devnet");
    kill_validators();
}

pub fn cleanup_devnet_only(devnet_validator: &mut Child) {
    cleanup_validator(devnet_validator, "devnet");
    kill_validators();
}

pub fn cleanup_validator(validator: &mut Child, label: &str) {
    eprintln!("Stopping {label} validator");
    stop_validator(validator, Duration::from_secs(10));
}

fn kill_process(name: &str) {
    process::Command::new("pkill")
        .arg("-15") // SIGTERM (default)
        .arg(name)
        .output()
        .unwrap();
    process::Command::new("pkill")
        .arg("-9") // Make sure it's really gone
        .arg(name)
        .output()
        .unwrap();
}

fn kill_validators() {
    // Makes sure all MagicBlock and Solana test validators are really killed.
    kill_process("magicblock-validator");
    kill_process("solana-test-validator");
}
