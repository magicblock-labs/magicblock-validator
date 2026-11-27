# crank-signer-check

Test program for scheduled tasks that sign with their crank signer. It fails
unless the crank signer of the given authority actually signed, then counts the
execution. The [Crank program tests](../magic-program/src/schedule_task/process_execute_crank.rs)
load its prebuilt `bin/crank_signer_check.so` into Engine.

Rebuild the binary after changing the source:

```sh
cargo build-sbf --manifest-path programs/crank-signer-check/Cargo.toml
cp target/deploy/crank_signer_check.so programs/crank-signer-check/bin/
```

[Back to workspace](../../README.md)
