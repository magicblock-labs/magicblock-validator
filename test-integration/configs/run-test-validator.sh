DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

solana-test-validator \
  --log \
  --rpc-port 7799 \
  -r \
  --account mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev \
  $DIR/accounts/validator-authority.json \
  --account LUzidNSiPNjYNkxZcUm5hYHwnWPwsUfh2US1cpWwaBm \
  $DIR/accounts/luzid-authority.json \
  --limit-ledger-size \
  1000000 \
  --bpf-program \
  DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh \
  $DIR/../schedulecommit/elfs/dlp.so \
  --bpf-program \
  9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY \
  $DIR/../target/deploy/program_schedulecommit.so \
  --bpf-program \
  4RaQH3CUBMSMQsSHPVaww2ifeNEEuaDZjF9CUdFwr3xr \
  $DIR/../target/deploy/program_schedulecommit_security.so

