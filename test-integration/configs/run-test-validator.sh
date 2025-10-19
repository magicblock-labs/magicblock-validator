DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

solana-test-validator \
  --log \
  --rpc-port 7799 \
  -r \
  --account tEsT3eV6RFCWs1BZ7AXTzasHqTtMnMLCB2tjQ42TDXD \
  $DIR/accounts/validator-authority.json \
  --account DUH8h7rYjdTPYyBUEGAUwZv9ffz5wiM45GdYWYzogXjp \
  $DIR/accounts/validator-fees-vault.json \
  --account 7JrkjmZPprHwtuvtuGTXp9hwfGYFAQLnLeFM52kqAgXg \
  $DIR/accounts/protocol-fees-vault.json \
  --account LUzidNSiPNjYNkxZcUm5hYHwnWPwsUfh2US1cpWwaBm \
  $DIR/accounts/luzid-authority.json \
  --limit-ledger-size \
  1000000 \
  --bpf-program \
  ComtrB2KEaWgXsW1dhr1xYL4Ht4Bjj3gXnnL6KMdABq \
  $DIR/../../target/deploy/magicblock_committor_program.so \
  --bpf-program \
  f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4 \
  $DIR/../target/deploy/program_flexi_counter.so \
  --bpf-program \
  DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh \
  $DIR/../schedulecommit/elfs/dlp.so \
  --bpf-program \
  9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY \
  $DIR/../target/deploy/program_schedulecommit.so \
  --bpf-program \
  4RaQH3CUBMSMQsSHPVaww2ifeNEEuaDZjF9CUdFwr3xr \
  $DIR/../target/deploy/program_schedulecommit_security.so

