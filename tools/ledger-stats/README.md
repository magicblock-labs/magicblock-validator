## Ledger Stats Tool

Provides diagnostics for ledger data stored in a local directory.

In order to run it directly from the repo use `cargo run --bin`, i.e.:

```sh
cargo run --bin ledger-stats -- log ./tools/ledger-stats/ledger/ --success
```

The examples use a globally installed version.

Implemented with subcommands, each of which have a help section, i.e.:

```sh
❯ ledger-stats log --help
ledger-stats-log 0.0.0
Transaction logs

USAGE:
    ledger-stats log [FLAGS] [OPTIONS] <ledger-path>

FLAGS:
    -h, --help       Prints help information
    -s, --success    Show successful transactions, default: false
    -V, --version    Prints version information

OPTIONS:
    -e, --end <end>        End slot
    -s, --start <start>    Start slot

ARGS:
    <ledger-path>
```

The idea is that we keep adding functionality as we need it in order to allow understanding
existing ledgers in order to diagnose user issues quickly.

We may also add a JSON output format should we ever want to build a UI around it.

### Summary

```sh
❯ ledger-stats --help
ledger-stats 0.0.0

USAGE:
    ledger-stats <SUBCOMMAND>

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

SUBCOMMANDS:
    count    Counts of items in ledger columns
    help     Prints this message or the help of the given subcommand(s)
    log      Transaction logs
    sig      Transaction details for signature
```

### count

Shown above

### log

```sh
❯ ledger-stats log ./tools/ledger-stats/ledger/ --success

Transaction: 2c1sRDHvvCCF58SVnrq3UnGSDdobHHbgccEgHUbuyzhU5ktgQ3pEXRHyR7JT5M7CUWStMfmRYEVSfLEJwa77Rn3X (4141)

  Program Magic11111111111111111111111111111111111111 invoke [1]
    • MutateAccounts: modifying '9twuZbSbuCjErSgMYxPimMKhS5AfhaKJjkdVfW3Ymyhe'.
    • MutateAccounts: setting lamports to 1712160
    • MutateAccounts: setting owner to zbtv2cgU1VzSBKNXZ96TcWSRVp1c8HxqCmRp8zPX1uh
    • MutateAccounts: setting executable to false
    • MutateAccounts: resolved data from id 1
    • MutateAccounts: setting data to len 118
    • MutateAccounts: setting rent_epoch to 18446744073709551615
    • Program Magic11111111111111111111111111111111111111 success

Transaction: 4cdgG62ut9Hav2WkKjwGgnMTFWGdw7g8gmkdU99CUGfvaR6MyXZZoh7G4CcWWEHmSek1BAfiHjHyKiE2a7U9mMcE (4141)
[ ... ]
```

### sig

<details>
<summary>Transaction details for signature</summary>

```sh
❯ ledger-stats sig ./tools/ledger-stats/ledger/ 3rEEV7SVSXuPYKrW2WgvkGj74mPzzrzbK4Y4dyRZGJYxQDLYiPMQWrQg4nNpwMG16Qc5Ye49jneWSehWJSyEMxH2

++++ Transaction Status ++++

Field                                                               Value
=====================                               =====================
Status                                                                 Ok
Fee                                                                     0
Pre-balances                            9,223,368,998,541,374,767 | 0 | 1
Post-balances           9,223,367,998,541,374,767 | 1,000,000,000,000 | 1
Inner Instructions                                                      0
Pre-token Balances                                                   None
Post-token Balances                                                  None
Rewards                                                              None
Loaded Addresses                                 writable: 0, readonly: 0
Return Data                                                          None
Compute Units Consumed                                                150


++++ Transaction Logs ++++

  Program Magic11111111111111111111111111111111111111 invoke [1]
    • MutateAccounts: modifying 'BPgkXhjdLUMstLjFvrbpG2TWXjSqqukgJ5GEiRnQNhAp'.
    • MutateAccounts: setting lamports to 1000000000000
    • MutateAccounts: setting owner to 11111111111111111111111111111111
    • MutateAccounts: setting executable to false
    • MutateAccounts: resolved data from id 15
    • MutateAccounts: setting data to len 0
    • MutateAccounts: setting rent_epoch to 0
    • Program Magic11111111111111111111111111111111111111 success

++++ Transaction ++++

num_required_signatures                  1
num_readonly_signed_accounts             0
num_readonly_unsigned_accounts           1
block_time                      1733731769

++++ Account Keys ++++

  • zbitnhqG6MLu3E6XBJGEd7WarnKDeqzriB14hr74Fjb
  • BPgkXhjdLUMstLjFvrbpG2TWXjSqqukgJ5GEiRnQNhAp
  • Magic11111111111111111111111111111111111111

++++ Instructions ++++

#1 Program ID: Magic11111111111111111111111111111111111111

  Accounts:
    • zbitnhqG6MLu3E6XBJGEd7WarnKDeqzriB14hr74Fjb
    • BPgkXhjdLUMstLjFvrbpG2TWXjSqqukgJ5GEiRnQNhAp

  Instruction Data Length: 106 (0x6a) bytes
  0000:   00 00 00 00  01 00 00 00  00 00 00 00  9a 64 97 d5
  0010:   a3 ff 66 a8  0f 03 9d cc  c2 06 f5 98  a1 e6 68 aa
  0020:   4a d5 6d d5  8b 68 ed 12  fc 65 4e 31  01 00 10 a5
  0030:   d4 e8 00 00  00 01 00 00  00 00 00 00  00 00 00 00
  0040:   00 00 00 00  00 00 00 00  00 00 00 00  00 00 00 00
  0050:   00 00 00 00  00 00 01 00  01 0f 00 00  00 00 00 00
  0060:   00 01 00 00  00 00 00 00  00 00
```
</details>

### accounts

The accounts subcommand provides details about the accounts in the ledger. It supports the
following options:

- -c, --count: Print the count of accounts instead of the account details.
- -r, --rent-epoch: Show the rent epoch for each account.
- -f, --filter <filter>...: Filter accounts based on specified criteria. Multiple criteria can be provided as a comma-separated list. Possible values include:
  - on or on-curve: Include on-curve accounts.
  - off or off-curve: Include off-curve accounts (PDAs).
  - executable: Include executable accounts.
  - non-executable: Include non-executable accounts.
- -o, --owner <owner>: Filter accounts by the specified owner.
- -s, --sort <sort>: Sort accounts by the specified column. The default is to sort by the account's public key (Pubkey).
The <ledger-path> argument specifies the path to the ledger directory.

Example usage:

```sh
❯ ledger-stats accounts ledger -s d -f=off
```

This will sort by data size and only include off-curve accounts.

### account

The account subcommand provides detailed information about a specific account in the ledger,
including its data. It requires two arguments:

- ledger-path: The path to the ledger directory.
- pubkey: The public key of the account to retrieve details for.

Example usage:

```sh
❯ ledger-stats account ledger 8JSRCegc3J5RqMp8izAZAs23PrmCg6e9TpraVB668xxn
```
