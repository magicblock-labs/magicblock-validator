
rm -rf ./test-ledger

solana-test-validator \
  --rpc-port 8899 \
  -r \
  --account mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev \
  ./configs/accounts/validator-authority.json \
  --account EpJnX7ueXk7fKojBymqmVuCuwyhDQsYcLVL1XMsBbvDX \
  ./configs/accounts/validator-fees-vault.json \
  --account 7JrkjmZPprHwtuvtuGTXp9hwfGYFAQLnLeFM52kqAgXg \
  ./configs/accounts/protocol-fees-vault.json \
  --account LUzidNSiPNjYNkxZcUm5hYHwnWPwsUfh2US1cpWwaBm \
  ./configs/accounts/luzid-authority.json \
  --limit-ledger-size \
  1000000 \
  --bpf-program \
  ComtrB2KEaWgXsW1dhr1xYL4Ht4Bjj3gXnnL6KMdABq \
  ../target/deploy/magicblock_committor_program.so \
  --bpf-program \
  f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4 \
  ./target/deploy/program_flexi_counter.so \
  --bpf-program \
  DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh \
  ./schedulecommit/elfs/dlp.so \
  --bpf-program \
  9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY \
  ./target/deploy/program_schedulecommit.so \
  --bpf-program \
  4RaQH3CUBMSMQsSHPVaww2ifeNEEuaDZjF9CUdFwr3xr \
  ./target/deploy/program_schedulecommit_security.so

