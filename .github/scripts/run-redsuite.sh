#!/usr/bin/env bash
set -euo pipefail
sudo prlimit --pid $$ --nofile=1048576:1048576
sudo sysctl fs.inotify.max_user_instances=1280
sudo sysctl fs.inotify.max_user_watches=655360
[ -n "${REDSUITE_CLONE_URL:-}" ] || unset REDSUITE_CLONE_URL
case "${1:-}" in
  checks) families=(redshift redhat) ;;
  benchmarks) families=(redline) ;;
  *) echo "Usage: $0 {checks|benchmarks}" >&2; exit 1 ;;
esac
family_first=false
if ./redsuite-dist/redsuite redshift --help >/dev/null 2>&1; then
  family_first=true
else
  export REDSUITE_VERBOSE=1
fi
status=0
for family in "${families[@]}"; do
  if [ "$family_first" = true ]; then
    args=("$family" run)
  else
    args=(run "$family")
  fi
  if [ "$family" = redline ]; then
    args+=(--profile lite)
  fi
  if ! /usr/bin/time -v ./redsuite-dist/redsuite "${args[@]}"; then
    status=1
  fi
done
exit "$status"
