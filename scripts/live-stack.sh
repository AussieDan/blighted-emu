#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Starts a local test world for the live panel: edge-http (HTTPS on port 8443), world-director,
# 1 to 8 sims, rep-gateway, and then blighted-admin in the foreground. Ctrl-C stops all of them.
# Usage: live-stack.sh [sims]   (default 4 sims of up to 4,000 players each, so 16,000 fit;
# run `cargo build --release` first; NW_GAME_DIR names the game install)
# LIVE_STACK_SNAPSHOT_MS sets the sim snapshot interval (default 250; 0 turns snapshots off, to
# measure what they cost).
# LIVE_STACK_TICK_THREADS sets the threads of each sim's mob and bundle steps (default 4; 0 = every
# core of the PC for each sim).
# It needs the git-ignored local files below. The servers get the secrets of the env file;
# blighted-admin does not (its bot batches read the env file themselves). Logs go to LOG_DIR.
# LIVE_STACK_ROOT overrides the repository folder (used by the tests).
# The control FIFO (LOG_DIR/control) takes one command per line: `restart-world` stops the
# world-director and the sims and starts them again (the Studio page's Apply uses it); the other
# programs keep running. Results go to LOG_DIR/control.log.
# LIVE_STACK_INSTANCE=2 (up to 9) starts a second test world next to the first: every port moves
# up by 100 per instance, the world id ends in the instance number, the logs go to
# local-data/logs/live-<instance>/, and the bots get their own names (edge config copied there).
set -euo pipefail

readonly EXIT_USAGE=2
readonly EXIT_MISSING=3
readonly INSTANCE="${LIVE_STACK_INSTANCE:-1}"
if [[ ! "$INSTANCE" =~ ^[1-9]$ ]]; then
  echo "live stack: LIVE_STACK_INSTANCE must be 1 to 9, not '$INSTANCE'" >&2
  exit "$EXIT_USAGE"
fi
readonly PORTS_PER_INSTANCE=100
readonly SHIFT=$(((INSTANCE - 1) * PORTS_PER_INSTANCE))
readonly FIRST_WORLD_ID="8d0f4c2e-0000-4000-8000-000000000001"
WORLD_ID="$(printf '8d0f4c2e-0000-4000-8000-%012d' "$INSTANCE")"
readonly WORLD_ID
# The director splits the map into strips of equal width, west to east. With 5 or more sims the
# westmost strip (x below 2,048 m) is sea with no start point, so that sim stays empty.
readonly DEFAULT_SIMS=4
readonly ROOT="${LIVE_STACK_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
readonly BIN="target/release"
readonly ENV_FILE="local-data/m3-test.env"
readonly EDGE_CONFIG="local-data/edge-http/edge.toml"
readonly DIRECTOR_CONFIG="local-data/world-director.toml"
readonly SOCIAL_CONFIG="config/social.toml"
if ((INSTANCE == 1)); then
  readonly LOG_DIR="local-data/logs/live"
else
  readonly LOG_DIR="local-data/logs/live-$INSTANCE"
fi
readonly CONTROL_FIFO="$LOG_DIR/control"
readonly CONTROL_LOG="$LOG_DIR/control.log"
readonly RESTART_WORLD="restart-world"
readonly EDGE_PORT=$((8443 + SHIFT))
readonly EDGE_LISTEN="0.0.0.0:$EDGE_PORT"
readonly EDGE_METRICS_LISTEN="127.0.0.1:$((9100 + SHIFT))"
# The bots send no region ping; a high port needs no right to bind.
readonly EDGE_PING_LISTEN="127.0.0.1:$((8080 + SHIFT))"
readonly GATEWAY_PORT=$((33435 + SHIFT))
readonly ADMIN_LISTEN="127.0.0.1:$((9180 + SHIFT))"
readonly PANEL_URL="http://$ADMIN_LISTEN/live"
# The sim metrics ports that local-data/nw-testclient.toml reads (port 9103 is the director's).
readonly SIM_METRICS_PORTS=(
  $((9102 + SHIFT)) $((9104 + SHIFT)) $((9105 + SHIFT)) $((9106 + SHIFT))
  $((9107 + SHIFT)) $((9108 + SHIFT)) $((9109 + SHIFT)) $((9110 + SHIFT))
)
readonly GATEWAY_METRICS_LISTEN="127.0.0.1:$((9101 + SHIFT))"
readonly DIRECTOR_METRICS_LISTEN="127.0.0.1:$((9103 + SHIFT))"
readonly SOCIAL_METRICS_LISTEN="127.0.0.1:$((9111 + SHIFT))"
readonly GATEWAY_METRICS_URL="http://$GATEWAY_METRICS_LISTEN/metrics"
readonly DIRECTOR_METRICS_URL="http://$DIRECTOR_METRICS_LISTEN/metrics"
# Bot names of a second world (letters only), so they never share an account with the first.
readonly INSTANCE_BOT_PREFIX="Instbot"
readonly DEFAULT_SNAPSHOT_INTERVAL_MS=250
readonly DEFAULT_SIM_TICK_THREADS=4
# Bots do not spread evenly over the sims' cells: each sim takes twice its usual 2,000.
readonly SIM_MAX_PLAYERS=4000
# Bots log in fast and all from this PC: room for 10,000 of them (the M5 load test settings).
readonly LOGIN_FAIL_LIMIT=50000
readonly GATEWAY_WORKERS=12
readonly GATEWAY_MAX_CONNECTIONS=12000
readonly HANDSHAKES_PER_IP_PER_SECOND=1000
# The sim joins after the director runs; the gateway starts after the sim has its cells.
readonly DIRECTOR_START_S=2
readonly SIM_START_S=8
readonly PROGRAMS=(edge-http world-director social sim rep-gateway blighted-admin nw-testclient)

sims="${1:-$DEFAULT_SIMS}"
if [[ ! "$sims" =~ ^[1-9][0-9]*$ ]] || ((sims > ${#SIM_METRICS_PORTS[@]})); then
  echo "live stack: sims must be 1 to ${#SIM_METRICS_PORTS[@]}, not '$sims'" >&2
  exit "$EXIT_USAGE"
fi

snapshot_interval_ms="${LIVE_STACK_SNAPSHOT_MS:-$DEFAULT_SNAPSHOT_INTERVAL_MS}"
if [[ ! "$snapshot_interval_ms" =~ ^[0-9]+$ ]]; then
  echo "live stack: LIVE_STACK_SNAPSHOT_MS must be a whole number of ms, not '$snapshot_interval_ms'" >&2
  exit "$EXIT_USAGE"
fi

sim_tick_threads="${LIVE_STACK_TICK_THREADS:-$DEFAULT_SIM_TICK_THREADS}"
if [[ ! "$sim_tick_threads" =~ ^[0-9]+$ ]]; then
  echo "live stack: LIVE_STACK_TICK_THREADS must be a whole number of threads, not '$sim_tick_threads'" >&2
  exit "$EXIT_USAGE"
fi

cd "$ROOT"

missing=()
for file in "$ENV_FILE" "$EDGE_CONFIG" "$DIRECTOR_CONFIG"; do
  [[ -f "$file" ]] || missing+=("$file")
done
for program in "${PROGRAMS[@]}"; do
  [[ -x "$BIN/$program" ]] || missing+=("$BIN/$program")
done
[[ -n "${NW_GAME_DIR:-}" ]] || missing+=("NW_GAME_DIR")
if ((${#missing[@]} > 0)); then
  echo "live stack: missing ${missing[*]}" >&2
  echo "Run 'cargo build --release', set NW_GAME_DIR, and keep the local-data files." >&2
  exit "$EXIT_MISSING"
fi
# The domain is only in the git-ignored edge config, never in this repository.
DOMAIN="$(sed -n 's/^domain *= *"\(.*\)" *$/\1/p' "$EDGE_CONFIG")"
if [[ -z "$DOMAIN" ]]; then
  echo "live stack: $EDGE_CONFIG names no domain (domain = \"<your domain>\")" >&2
  exit "$EXIT_MISSING"
fi
readonly DOMAIN
readonly LOGIN_URL="https://login.$DOMAIN:$EDGE_PORT"
readonly GAME_URL="https://game.$DOMAIN:$EDGE_PORT"

mkdir -p "$LOG_DIR"
pids=()
stop_all() {
  kill "${pids[@]}" 2>/dev/null || true
  wait "${pids[@]}" 2>/dev/null || true
  rm -f "$CONTROL_FIFO"
}
trap stop_all EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# 1 after a restart-world: the restarted servers add to their logs instead of starting new ones,
# so the log of a run before an Apply stays.
append_logs=0

# Starts one server with the secrets of the env file; its output goes to LOG_DIR/<name>.log.
start_server() {
  local name="$1"
  shift
  local log="$LOG_DIR/$name.log"
  if ((append_logs)); then
    echo "=== $RESTART_WORLD $(date -Is) ===" >>"$log"
  else
    : >"$log"
  fi
  (
    set -a
    # shellcheck source=/dev/null
    . "$ENV_FILE"
    set +a
    exec "$@"
  ) >>"$log" 2>&1 &
  pids+=("$!")
}

# Starts the world-director, the social service and the sims; their pids go to `pids` (in the world loop's shell).
start_world() {
  start_server director env NW_DIRECTOR_WORLD_ID="$WORLD_ID" \
    NW_DIRECTOR_METRICS_LISTEN="$DIRECTOR_METRICS_LISTEN" \
    "$BIN/world-director" --config "$DIRECTOR_CONFIG"
  start_server social env NW_SOCIAL_WORLD_ID="$WORLD_ID" NW_SOCIAL_GAME_DIR="$NW_GAME_DIR" \
    NW_SOCIAL_METRICS_LISTEN="$SOCIAL_METRICS_LISTEN" \
    "$BIN/social" --config "$SOCIAL_CONFIG"
  sleep "$DIRECTOR_START_S"
  local index
  for ((index = 0; index < sims; index++)); do
    local sim_id="sim-$((index + 1))"
    start_server "$sim_id" env NW_SIM_SIM_ID="$sim_id" NW_SIM_WORLD_ID="$WORLD_ID" \
      NW_SIM_METRICS_LISTEN="127.0.0.1:${SIM_METRICS_PORTS[index]}" \
      NW_SIM_TICK_THREADS="$sim_tick_threads" NW_SIM_MAX_PLAYERS="$SIM_MAX_PLAYERS" \
      NW_SIM_ADMIN__SNAPSHOT_INTERVAL_MS="$snapshot_interval_ms" "$BIN/sim"
  done
}

# Runs in the background: owns the director and the sims, and reads the control FIFO. The FIFO
# is opened read-write so the open never blocks and a stop signal ends the read at once.
world_loop() {
  pids=()
  trap 'kill "${pids[@]}" 2>/dev/null || true; wait "${pids[@]}" 2>/dev/null || true; exit 0' TERM
  start_world
  exec 3<>"$CONTROL_FIFO"
  local line
  while read -r line <&3; do
    if [[ "$line" == "$RESTART_WORLD" ]]; then
      kill "${pids[@]}" 2>/dev/null || true
      wait "${pids[@]}" 2>/dev/null || true
      pids=()
      append_logs=1
      start_world
      echo "$RESTART_WORLD done" >>"$CONTROL_LOG"
    else
      echo "unknown: $line" >>"$CONTROL_LOG"
    fi
  done
}

# A second world gets a copy of the edge config with its world id and gateway port.
edge_config="$EDGE_CONFIG"
if ((INSTANCE > 1)); then
  edge_config="$LOG_DIR/edge.toml"
  sed -e "s/$FIRST_WORLD_ID/$WORLD_ID/" \
    -e "s/127\.0\.0\.1:33435/127.0.0.1:$GATEWAY_PORT/" "$EDGE_CONFIG" >"$edge_config"
fi
start_server edge env NW_EDGE_HTTPS_LISTEN="$EDGE_LISTEN" \
  NW_EDGE_PING_LISTEN="$EDGE_PING_LISTEN" NW_EDGE_METRICS_LISTEN="$EDGE_METRICS_LISTEN" \
  NW_EDGE_AUTH__LOGIN_FAIL_LIMIT="$LOGIN_FAIL_LIMIT" \
  "$BIN/edge-http" --config "$edge_config" serve
rm -f "$CONTROL_FIFO"
mkfifo "$CONTROL_FIFO"
world_loop &
pids+=("$!")
metrics_urls="\"$GATEWAY_METRICS_URL\", \"$DIRECTOR_METRICS_URL\""
for ((index = 0; index < sims; index++)); do
  metrics_urls+=", \"http://127.0.0.1:${SIM_METRICS_PORTS[index]}/metrics\""
done
sleep "$((DIRECTOR_START_S + SIM_START_S))"
start_server gateway env NW_GATEWAY_WORKERS="$GATEWAY_WORKERS" \
  NW_GATEWAY_WORLD_ID="$WORLD_ID" NW_GATEWAY_UDP_LISTEN="0.0.0.0:$GATEWAY_PORT" \
  NW_GATEWAY_METRICS_LISTEN="$GATEWAY_METRICS_LISTEN" \
  NW_GATEWAY_LIMITS__MAX_CONNECTIONS="$GATEWAY_MAX_CONNECTIONS" \
  NW_GATEWAY_LIMITS__HANDSHAKES_PER_IP_PER_SECOND="$HANDSHAKES_PER_IP_PER_SECOND" \
  "$BIN/rep-gateway" serve

echo "$sims sims run; logs in $LOG_DIR. Open $PANEL_URL. Ctrl-C stops everything."
# blighted-admin runs in the background and the script waits for it, so a stop signal reaches
# the traps at once.
admin_env=(
  NW_TESTCLIENT_LOGIN_URL="$LOGIN_URL" NW_TESTCLIENT_GAME_URL="$GAME_URL"
  BLIGHTED_ADMIN_LISTEN="$ADMIN_LISTEN" BLIGHTED_ADMIN_LIVE__WORLD_ID="$WORLD_ID"
  BLIGHTED_ADMIN_LIVE__METRICS_URLS="[$metrics_urls]"
  BLIGHTED_ADMIN_STUDIO__CONTROL_FIFO="$CONTROL_FIFO"
)
if ((INSTANCE > 1)); then
  admin_env+=(
    BLIGHTED_ADMIN_LIVE__BOT_ACCOUNT_PREFIX="${INSTANCE_BOT_PREFIX,,}"
    BLIGHTED_ADMIN_LIVE__BOT_CHARACTER_PREFIX="$INSTANCE_BOT_PREFIX"
  )
fi
env "${admin_env[@]}" "$BIN/blighted-admin" serve &
pids+=("$!")
wait "$!"
