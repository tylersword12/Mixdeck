#!/usr/bin/env bash
# Runs a command, and if it fails, repeats the end of its output as a GitHub
# annotation so the failure reason shows on the run's summary page.
log="$(mktemp)"
"$@" 2>&1 | tee "$log"
code=${PIPESTATUS[0]}
if [ "$code" -ne 0 ]; then
  msg="$(grep -v '^\s*$' "$log" | tail -n 60 | sed 's/\x1b\[[0-9;]*m//g' | python3 -c 'import sys;print(sys.stdin.read().replace("%","%25").replace("\r","").replace("\n","%0A"))')"
  echo "::error title=$1 failed (exit $code)::$msg"
fi
exit "$code"
