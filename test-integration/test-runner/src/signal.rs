use std::{
    error::Error,
    process::{self, Output},
    sync::mpsc::channel,
};

use crate::cleanup::cleanup_validator;

pub fn wait_for_ctrlc(
    devnet_validator: Option<process::Child>,
    ephem_validator: Option<process::Child>,
    output: Output,
) -> Result<Output, Box<dyn Error>> {
    let (tx, rx) = channel();
    ctrlc::set_handler(move || {
        tx.send(()).expect("Could not send signal on channel.")
    })?;

    println!("Hit Ctrl-C to stop validator(s)...");
    rx.recv().expect("Could not receive from channel.");

    if let Some(mut validator) = devnet_validator {
        cleanup_validator(&mut validator, "devnet");
    }
    if let Some(mut validator) = ephem_validator {
        cleanup_validator(&mut validator, "ephemeral");
    }

    Ok(output)
}
