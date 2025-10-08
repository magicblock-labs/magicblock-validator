## Integration Tests

### Running Tests

To run all tests automatically, use the following command:

```bash
make test
```

In order to isolate issues you can run one set of the below (each command in its own terminal):
You an then also run individual tests of the respective suite while keeping the setup
validators running in the other terminals.

```sh
make setup-schedulecommit-devnet
make setup-schedulecommit-ephem
cargo nextest run -p schedulecommit-test-scenarios --no-fail-fast -j16
cargo nextest run -p schedulecommit-test-security --no-fail-fast -j16
```

```sh
make setup-chainlink-devnet
cargo nextest run -p test-chainlink --no-fail-fast -j16
```

```sh
make setup-cloning-devnet
make setup-cloning-ephem
cargo nextest run -p test-cloning --no-fail-fast -j16

```sh
make setup-restore-ledger-devnet
cargo nextest run -p test-ledger-restore --no-fail-fast -j16
```

```sh
make setup-magicblock-api-devnet
make setup-magicblock-api-ephem
cargo nextest run -p test-magicblock-api --no-fail-fast -j16
```

```sh
make setup-table-mania-devnet
cargo nextest run -p test-table-mania --no-fail-fast -j16
```

```sh
make setup-committor-devnet
cargo nextest run -p schedulecommit-committor-service --no-fail-fast -j16
```

```sh
make setup-pubsub-devnet
make setup-pubsub-ephem
cargo nextest run -p test-pubsub --no-fail-fast -j16
```

```sh
make setup-config-devnet
cargo nextest run -p test-config --no-fail-fast -j16
```

```sh
make setup-schedule-intents-devnet
make setup-schedule-intents-ephem
cargo nextest run -p test-schedule-intent --no-fail-fast -j16
```
