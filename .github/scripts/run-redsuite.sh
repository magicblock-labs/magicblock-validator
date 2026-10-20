#!/usr/bin/env bash
set -euo pipefail
family=${1:-}
case "$family" in
  redshift|redhat|redline) ;;
  *) echo "Usage: $0 {redshift|redhat|redline}" >&2; exit 1 ;;
esac
sudo prlimit --pid $$ --nofile=1048576:1048576
sudo sysctl fs.inotify.max_user_instances=1280
sudo sysctl fs.inotify.max_user_watches=655360
[ -n "${REDSUITE_CLONE_URL:-}" ] || unset REDSUITE_CLONE_URL
if ./redsuite-dist/redsuite redshift --help >/dev/null 2>&1; then
  args=("$family" run)
else
  export REDSUITE_VERBOSE=1
  args=(run "$family")
fi
if [ "$family" = redline ]; then
  args+=(--profile lite)
fi
exec /usr/bin/time -v ./redsuite-dist/redsuite "${args[@]}"
