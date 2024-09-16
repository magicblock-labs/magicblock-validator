DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

solana-test-validator \
  --rpc-port 7799 \
  -r \
  --limit-ledger-size \
  10000 \
  --bpf-program \
  DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh \
  $DIR/../elfs/dlp.so \
  --bpf-program \
  9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY \
  $DIR/../elfs/schedulecommit_program.so \
  --bpf-program \
  4RaQH3CUBMSMQsSHPVaww2ifeNEEuaDZjF9CUdFwr3xr \
  $DIR/../elfs/schedulecommit_test_security.so
