#!/usr/bin/env bash
set -euo pipefail

# This script fetches program .so binaries and account .json files into
# <package-root>/bin/local-dumps, to be used by mb-test-validator.
# It mirrors the list from the issue description and defaults to MagicBlock devnet RPC.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PKG_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DUMPS_DIR="$PKG_ROOT/lib/bin/local-dumps"
RPC_URL="${SOLANA_RPC_URL:-https://rpc.magicblock.app/devnet}"

mkdir -p "$DUMPS_DIR"

# Dump accounts
accounts=(
  mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev
  EpJnX7ueXk7fKojBymqmVuCuwyhDQsYcLVL1XMsBbvDX
  7JrkjmZPprHwtuvtuGTXp9hwfGYFAQLnLeFM52kqAgXg
  Cuj97ggrhhidhbu39TijNVqE74xvKJ69gDervRUXAxGh
  5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc
  F72HqCR8nwYsVyeVd38pgKkjXmXFzVAM8rjZZsXWbdE
)

for acc in "${accounts[@]}"; do
  out="$DUMPS_DIR/$acc.json"
  echo "Dumping account $acc -> $out"
  if ! solana account "$acc" --output json --url "$RPC_URL" > "$out"; then
    echo "Warning: failed to dump account $acc" >&2
  fi
done

# Dump programs
programs=(
  DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh
  noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV
  Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz
  DmnRGfyyftzacFb1XadYhWF6vWqXwtQk5tbr6XgR3BA1
)

for prog in "${programs[@]}"; do
  out="$DUMPS_DIR/$prog.so"
  echo "Dumping program $prog -> $out"
  if ! solana program dump "$prog" "$out" --url "$RPC_URL"; then
    echo "Warning: failed to dump program $prog" >&2
  fi
done

echo "local-dumps directory: $DUMPS_DIR"
