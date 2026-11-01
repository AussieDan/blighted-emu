#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests for live-stack.sh: it starts nothing when a file, a program or NW_GAME_DIR is missing.
set -euo pipefail

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly STACK_SCRIPT="$SCRIPT_DIR/live-stack.sh"
readonly EXIT_USAGE=2
readonly EXIT_MISSING=3

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

failures=0

expect() {
  local name="$1" expected_exit="$2" expected_text="$3"
  shift 3
  local actual=0 output
  output="$("$@" 2>&1)" || actual=$?
  if [[ "$actual" -eq "$expected_exit" && "$output" == *"$expected_text"* ]]; then
    echo "PASS: $name"
  else
    echo "FAIL: $name (exit $actual, output: $output)"
    failures=$((failures + 1))
  fi
}

expect "an empty folder names the missing env file" "$EXIT_MISSING" "local-data/m3-test.env" \
  env LIVE_STACK_ROOT="$work_dir" NW_GAME_DIR="$work_dir" "$STACK_SCRIPT"

expect "an empty folder names a missing program" "$EXIT_MISSING" "target/release/sim" \
  env LIVE_STACK_ROOT="$work_dir" NW_GAME_DIR="$work_dir" "$STACK_SCRIPT"

expect "no game folder is named" "$EXIT_MISSING" "NW_GAME_DIR" \
  env -u NW_GAME_DIR LIVE_STACK_ROOT="$work_dir" "$STACK_SCRIPT"

expect "nine sims are refused" "$EXIT_USAGE" "sims must be 1 to 8" \
  env LIVE_STACK_ROOT="$work_dir" NW_GAME_DIR="$work_dir" "$STACK_SCRIPT" 9

expect "a sim count that is not a number is refused" "$EXIT_USAGE" "not 'five'" \
  env LIVE_STACK_ROOT="$work_dir" NW_GAME_DIR="$work_dir" "$STACK_SCRIPT" five

expect "a snapshot interval that is not a number is refused" "$EXIT_USAGE" "not 'often'" \
  env LIVE_STACK_ROOT="$work_dir" NW_GAME_DIR="$work_dir" LIVE_STACK_SNAPSHOT_MS=often \
  "$STACK_SCRIPT"

expect "a thread count that is not a number is refused" "$EXIT_USAGE" "not 'many'" \
  env LIVE_STACK_ROOT="$work_dir" NW_GAME_DIR="$work_dir" LIVE_STACK_TICK_THREADS=many \
  "$STACK_SCRIPT"

if [[ -d "$work_dir/local-data/logs" ]]; then
  echo "FAIL: a refused start wrote logs"
  failures=$((failures + 1))
else
  echo "PASS: a refused start writes nothing"
fi

# A fake stack: every program writes its pid and sleeps. The control FIFO restarts the
# director and the sims; the other programs keep their pids.
readonly WAIT_STEPS=300
readonly WAIT_STEP_S=0.1
readonly FAKE_SLEEP_S=600

wait_for() {
  local step
  for ((step = 0; step < WAIT_STEPS; step++)); do
    if "$@"; then
      return 0
    fi
    sleep "$WAIT_STEP_S"
  done
  return 1
}

fake_root="$work_dir/fake"
mkdir -p "$fake_root/target/release" "$fake_root/local-data/edge-http" "$fake_root/pids"
touch "$fake_root/local-data/m3-test.env" "$fake_root/local-data/edge-http/edge.toml" \
  "$fake_root/local-data/world-director.toml"
for program in edge-http world-director social sim rep-gateway blighted-admin nw-testclient; do
  printf '#!/usr/bin/env bash\necho "$*" > "%s/pids/%s.args"\nenv > "%s/pids/%s.env"\necho $$ > "%s/pids/%s"\necho "started $$"\nexec sleep %s\n' \
    "$fake_root" "$program" "$fake_root" "$program" "$fake_root" "$program" "$FAKE_SLEEP_S" \
    >"$fake_root/target/release/$program"
  chmod +x "$fake_root/target/release/$program"
done
control="$fake_root/local-data/logs/live/control"
control_log="$fake_root/local-data/logs/live/control.log"
setsid env LIVE_STACK_ROOT="$fake_root" NW_GAME_DIR="$fake_root" "$STACK_SCRIPT" 1 \
  >"$work_dir/stack.out" 2>&1 &
stack_pid=$!
if wait_for test -s "$fake_root/pids/blighted-admin" && wait_for test -p "$control"; then
  before_edge="$(cat "$fake_root/pids/edge-http")"
  before_director="$(cat "$fake_root/pids/world-director")"
  before_sim="$(cat "$fake_root/pids/sim")"
  echo "restart-world" >"$control"
  if wait_for grep -qs "restart-world done" "$control_log" &&
    [[ "$(cat "$fake_root/pids/world-director")" != "$before_director" ]] &&
    [[ "$(cat "$fake_root/pids/sim")" != "$before_sim" ]] &&
    [[ "$(cat "$fake_root/pids/edge-http")" == "$before_edge" ]]; then
    echo "PASS: restart-world restarts the director and the sims only"
  else
    echo "FAIL: restart-world (log: $(cat "$control_log" 2>/dev/null))"
    failures=$((failures + 1))
  fi
  sim_log="$fake_root/local-data/logs/live/sim-1.log"
  if [[ "$(grep -c '^started ' "$sim_log")" == 2 ]] && grep -q "^=== restart-world " "$sim_log"; then
    echo "PASS: a restarted sim adds to its log"
  else
    echo "FAIL: a restarted sim adds to its log ($(cat "$sim_log" 2>/dev/null))"
    failures=$((failures + 1))
  fi
  echo "dance" >"$control"
  if wait_for grep -qs "unknown: dance" "$control_log"; then
    echo "PASS: an unknown control line is logged"
  else
    echo "FAIL: an unknown control line is logged"
    failures=$((failures + 1))
  fi
else
  echo "FAIL: the fake stack did not start (output: $(cat "$work_dir/stack.out"))"
  failures=$((failures + 1))
fi
kill -TERM "$stack_pid" 2>/dev/null || true
wait "$stack_pid" 2>/dev/null || true
if wait_for test ! -e "$control" &&
  wait_for test -z "$(pgrep -g "$stack_pid" 2>/dev/null)"; then
  echo "PASS: a stopped stack removes the control FIFO and stops every program"
else
  echo "FAIL: a stopped stack removes the control FIFO and stops every program"
  failures=$((failures + 1))
fi
# Whatever a failed run left behind.
kill -TERM -- "-$stack_pid" 2>/dev/null || true

# A hang-up (the terminal or the shell around the script went away) also stops every program.
rm -f "$fake_root/pids/"*
setsid env LIVE_STACK_ROOT="$fake_root" NW_GAME_DIR="$fake_root" "$STACK_SCRIPT" 1 \
  >"$work_dir/stack-hup.out" 2>&1 &
stack_pid=$!
if wait_for test -s "$fake_root/pids/blighted-admin"; then
  kill -HUP "$stack_pid" 2>/dev/null || true
  wait "$stack_pid" 2>/dev/null || true
  if wait_for test -z "$(pgrep -g "$stack_pid" 2>/dev/null)"; then
    echo "PASS: a hang-up stops every program"
  else
    echo "FAIL: a hang-up stops every program"
    failures=$((failures + 1))
  fi
else
  echo "FAIL: the fake stack for the hang-up test did not start"
  failures=$((failures + 1))
fi
kill -TERM -- "-$stack_pid" 2>/dev/null || true

expect "an instance that is not 1 to 9 is refused" "$EXIT_USAGE" "LIVE_STACK_INSTANCE" \
  env LIVE_STACK_ROOT="$work_dir" NW_GAME_DIR="$work_dir" LIVE_STACK_INSTANCE=10 "$STACK_SCRIPT"

# A second instance: its own ports, world id, logs, edge config copy and bot names.
rm -f "$fake_root/pids/"*
echo 'id = "8d0f4c2e-0000-4000-8000-000000000001"' >"$fake_root/local-data/edge-http/edge.toml"
setsid env LIVE_STACK_ROOT="$fake_root" NW_GAME_DIR="$fake_root" LIVE_STACK_INSTANCE=2 \
  "$STACK_SCRIPT" 1 >"$work_dir/stack2.out" 2>&1 &
stack_pid=$!
second_world="8d0f4c2e-0000-4000-8000-000000000002"
if wait_for test -s "$fake_root/pids/blighted-admin" &&
  grep -q "NW_SIM_WORLD_ID=$second_world" "$fake_root/pids/sim.env" &&
  grep -q "NW_SIM_METRICS_LISTEN=127.0.0.1:9202" "$fake_root/pids/sim.env" &&
  grep -q "BLIGHTED_ADMIN_LISTEN=127.0.0.1:9280" "$fake_root/pids/blighted-admin.env" &&
  grep -q "BLIGHTED_ADMIN_LIVE__BOT_CHARACTER_PREFIX=Instbot" "$fake_root/pids/blighted-admin.env" &&
  grep -q "NW_GATEWAY_UDP_LISTEN=0.0.0.0:33535" "$fake_root/pids/rep-gateway.env" &&
  grep -q "local-data/logs/live-2/edge.toml" "$fake_root/pids/edge-http.args" &&
  grep -q "$second_world" "$fake_root/local-data/logs/live-2/edge.toml" &&
  test -p "$fake_root/local-data/logs/live-2/control"; then
  echo "PASS: a second instance gets its own ports, world id, logs and bot names"
else
  echo "FAIL: a second instance gets its own ports, world id, logs and bot names (output: $(cat "$work_dir/stack2.out"))"
  failures=$((failures + 1))
fi
kill -TERM "$stack_pid" 2>/dev/null || true
wait "$stack_pid" 2>/dev/null || true
kill -TERM -- "-$stack_pid" 2>/dev/null || true

if [[ "$failures" -gt 0 ]]; then
  echo "$failures test(s) failed"
  exit 1
fi
echo "all live-stack tests passed"
