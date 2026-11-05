#!/usr/bin/env bash
set -euo pipefail
family=${1:-}
case "$family" in
  redshift|redshift-dlp|redhat|redline) ;;
  *) echo "Usage: $0 {redshift|redshift-dlp|redhat|redline}" >&2; exit 1 ;;
esac
sudo prlimit --pid $$ --nofile=1048576:1048576
sudo sysctl -q fs.inotify.max_user_instances=1280
sudo sysctl -q fs.inotify.max_user_watches=655360
[ -n "${REDSUITE_CLONE_URL:-}" ] || unset REDSUITE_CLONE_URL
args=("$family" run)
if [ "$family" = redshift-dlp ]; then
  args=(redshift run --serial dlp_v2_lifecycle dlp_v2_buffered_settlement dlp_mixed_settlement)
elif [ "$family" = redshift ]; then
  args+=(--fresh-concurrency 1)
fi
if [ "$family" = redline ]; then
  args+=(--profile lite --duration 30s)
fi
exec ./redsuite-dist/redsuite "${args[@]}"
