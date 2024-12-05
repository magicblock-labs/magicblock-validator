use std::process::{self, Child};

pub fn cleanup_validators(ephem_validator: &mut Child, devnet_validator: &mut Child) {
    cleanup_validator(ephem_validator, "ephemeral");
    cleanup_validator(devnet_validator, "devnet");
    kill_validators();
}

pub fn cleanup_devnet_only(devnet_validator: &mut Child) {
    cleanup_validator(devnet_validator, "devnet");
    kill_validators();
}

fn cleanup_validator(validator: &mut Child, label: &str) {
    validator.kill().unwrap_or_else(|err| {
        panic!("Failed to kill {} validator ({:?})", label, err)
    });
}

fn kill_process(name: &str) {
    process::Command::new("pkill")
        .arg("-9")
        .arg(name)
        .output()
        .unwrap();
}

fn kill_validators() {
    // Makes sure all the rpc + solana teset validators  are really killed
    kill_process("rpc");
    kill_process("solana-test-validator");
}
