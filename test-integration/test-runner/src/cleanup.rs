use std::process::{self, Child};

pub fn cleanup_validators(
    ephem_validator: &mut Child,
    devnet_validator: &mut Child,
    uses_light_validator: bool,
) {
    cleanup_validator(ephem_validator, "ephemeral");
    if uses_light_validator {
        cleanup_light_validator(devnet_validator);
    } else {
        cleanup_validator(devnet_validator, "devnet");
    }
    kill_validators();
}

pub fn cleanup_devnet_only(devnet_validator: &mut Child) {
    cleanup_validator(devnet_validator, "devnet");
    kill_validators();
}

pub fn cleanup_validator(validator: &mut Child, label: &str) {
    validator.kill().unwrap_or_else(|err| {
        panic!("Failed to kill {} validator ({:?})", label, err)
    });
}

pub fn cleanup_light_validator(validator: &mut Child) {
    validator.kill().unwrap_or_else(|err| {
        panic!("Failed to kill light validator ({:?})", err)
    });
    let command = process::Command::new("light")
        .arg("test-validator")
        .arg("--stop")
        .output()
        .unwrap();
    if !command.status.success() {
        panic!("Failed to stop light validator: {:?}", command);
    }
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
    // Makes sure all the magicblock-validator + solana test validators  are really killed
    kill_process("magicblock-validator");
    kill_process("solana-test-validator");
}
