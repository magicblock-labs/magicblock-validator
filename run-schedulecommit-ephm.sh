#!/bin/bash  

set -x

# cd test-integration && cargo build-sbf && cd - 

RUST_LOG=info RUST_LOG_STYLE="EPHEM" VALIDATOR_KEYPAIR="62LxqpAW6SWhp7iKBjCQneapn1w6btAhW7xHeREWSpPzw3xZbHCfAFesSR4R76ejQXCLWrndn37cKCCLFvx6Swps" "cargo" "run" "--" "/Users/snawaz/projects/mb/magicblock-validator/test-integration/configs/schedulecommit-conf-fees.ephem.toml"
