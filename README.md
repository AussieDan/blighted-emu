# Blighted New World Emulator

A preservation server emulator for New World: Aeternum. The official servers shut down on 2027-01-31.

- Players use their own legal copy of the final PC client. This project never distributes Amazon game files.
- The emulator runs on standard dedicated servers, with no AWS dependency.
- It is non-commercial.

Design: the architecture spec is kept locally (not in git).
Research: [docs/NEW_WORLD_EMULATOR_RESEARCH.md](docs/NEW_WORLD_EMULATOR_RESEARCH.md)
Progress: [docs/progress.html](docs/progress.html): what is done, what is missing, and what comes next. Open it in a browser.
Client patch: [docs/client-patch.md](docs/client-patch.md): how to make `BlightedNewWorld.exe` (cert bypass) so the real client can enter the emulator, and how to re-make it after a game update.

## Licences

| Part | Licence |
|---|---|
| Server programs and core crates | AGPL-3.0-or-later ([LICENSE](LICENSE)) |
| `launcher` | GPL-3.0-or-later |
| `nw-formats` | MIT OR Apache-2.0 |
| `nw-oodle` | GPL-3.0-or-later |

Each crate states its licence in its `Cargo.toml`. `cargo deny check` enforces the policy in `deny.toml`.

## Requirements

- Rust (the version in `rust-toolchain.toml` is installed automatically by rustup)
- Docker with Compose v2
- Node.js (`just test-web` uses its built-in test runner; no npm packages)
- Python 3 (`scripts/patch-client.py` and its test)
- `cargo install --locked cargo-deny cargo-sweep just`

## Quick start

```bash
cp deploy/.env.example deploy/.env
```

Edit `deploy/.env` and set new passwords. Then:

```bash
just check
just infra-up
just infra-test
```

## Commands

| Command | What it does |
|---|---|
| `just check` | Game-data guard, format, lint, test, dependency policy, script tests, web tests |
| `just test-scripts` | Tests of the shell and Python scripts in `scripts/` |
| `just test-web` | Tests of the admin page code (`bins/blighted-admin/web/*.test.mjs`) |
| `just size` | Prints the build folder size; fails above 10 GiB |
| `just clean-old` | Deletes build files older than 7 days |
| `just infra-up` / `just infra-down` | Starts / stops PostgreSQL, Valkey, NATS, Prometheus, Grafana |
| `just infra-test` | Infrastructure smoke test |
| `just test-infra` | Tests that need PostgreSQL, Valkey and NATS, with the login, gateway and world walk (run `just infra-up` first) |
| `just test-local` | Tests that read your New World install (`NW_GAME_DIR`) |
| `just codegen` | Regenerates the table structs in `crates/nw-content/src/generated` |
| `just bundle` | Builds the retail content bundle into `local-data/content/retail/` |
| `just world` | Builds one retail world file per level into `local-data/content/retail/` (run `just bundle` first) |
| `just design` | Merges the design files of `content/` over the retail files into `local-data/content/` (run `just bundle` and `just world` first) |
| `just check-design` | Checks the design files against the retail files; writes nothing |
| `just collision` | Builds the collision files of the tutorial and the open world into `local-data/content/` |
| `just live <sims>` | Starts a local test world and the live panel (see "Live panel") |
| `just capture-check` | Decodes every capture ledger under `local-data/captures/` and prints counts |
| `just map-tiles` | Writes the open world map tiles into `local-data/content/map/` from your install |

## Content data

The server reads game rules from your own New World install. Nothing from the game is stored in this repository; only the table shapes (names and types) are generated into `crates/nw-content/src/generated`.

```bash
export NW_GAME_DIR="/path/to/steamapps/common/New World"
just bundle
just world
just design
just collision
```

`just bundle` writes the retail bundle `local-data/content/retail/bundle-<build>.bin`, then reloads it and checks every table and row. The pinned client build is in `config/client-build.toml`.

`just world` writes the retail world files `local-data/content/retail/world-<build>-<level>.bin` for each of the 41 levels (about 620 MB in all), reloads each file, and prints the record counts, the skipped metadata fields, and the spawn IDs that no datasheet row has.

`just design` reads the retail files, merges the team's design files from `content/` over them (see [content/README.md](content/README.md)), and writes `local-data/content/bundle-<build>.bin` and `local-data/content/world-<build>-<level>.bin`: the files the servers and the admin tools read. It does not read the game install. With no design files, the merged files are equal to the retail files. When a design file has a problem, it prints each one and writes nothing.

`just collision` writes the level collision files `local-data/content/collision-<build>-<level>.bin` of the tutorial and the open world (about 104 MB in all): the solid shapes of each level's placed slices (variant capitals as their variation data shows them, and the slices encounters spawn later), read from the game's physics assets (`.rnr`), which the sims stand the players and mobs on. A sim does not start without the collision file of its level. See `docs/re/2026-09-20-level-collision.md`.

A sim needs only the bundle, the world file and the collision file of its level. It also reads the files below from `content_dir` when they exist. When one is missing, the sim logs a warning and runs without that part.

| File | Made by | Without it |
|---|---|---|
| `roads-<build>-<level>.bin` | `cargo run --release -p content-tool -- roads [levels]` (default `newworld_vitaeeterna`) | No road sprint boost |
| `water-<build>-<level>.bin` | `cargo run --release -p content-tool -- water [levels]` (default `ftue_v2` and `newworld_vitaeeterna`) | The sea level is the only water |
| `player-attacks-<build>.json` | `cargo run --release -p content-tool -- player-attacks` | Players cannot attack |
| `player-sequences-<build>.json` | No tool. It was mined from capture ledgers; a copy of the pinned build's table is `bins/sim/src/world/testdata/player-sequences-25810808.json` | The server starts no attack, block, aim, dodge or jump |
| `mob-prefabs-<build>.json` | `cargo run --release -p content-tool -- mob-prefabs [folders]` (reads the ledgers in `local-data/captures` and `local-data/open world captures`) | Mobs are not shown to clients |
| `creature-animations-<build>.json` | `cargo run --release -p content-tool -- creature-animations` (needs the mob prefab table) | Mobs move without animations |
| `ai-data-<build>.bin` | `cargo run --release -p content-tool -- ai-data` (reads `local-data/server-pak/Server.pak`; needs the creature animation table) | Every mob stands at its spawn |
| `ftue-entities-<build>.json` | `cargo run --release -p content-tool -- ftue-entities <ledger.bin>` (a capture of a new character's tutorial) | The tutorial's level entities are not sent |
| `dungeon-scripts-<build>.json` | `cargo run --release -p content-tool -- dungeon-scripts` | Instances run without encounters |

The tools that take a bundle read the merged bundle `local-data/content/bundle-<build>.bin`, so run `just design` first. The tools that read ledgers (`mob-prefabs`, `ftue-entities`) use a lot of memory; run them under a limit, for example `ulimit -v 4000000`. `cargo run -p content-tool -- --help` lists every subcommand.

### Recovery

`content-tool` links `nw-content`, so it cannot run while the generated code does not compile. Removing the `generated` module lines from `crates/nw-content/src/lib.rs` is not enough, because `bundle.rs` and `content-tool build` use `Tables`. Instead:

1. Restore the committed generated files: `git checkout -- crates/nw-content/src/generated`.
2. Fix the cause (usually a change to `bins/content-tool/src/codegen`).
3. Run `just codegen` again.

Codegen stops with an error when a table type name would give an invalid Rust name or a name that the generated code already uses, so a new client build cannot write code that does not compile for that reason.

## Capture check

`content-tool capture-check` reads capture ledgers (`ledger.bin`, decrypted carrier datagrams) with `nw-capture`, decodes them with `nw-carrier` and `nw-reflect`, and prints counts only: datagrams, messages per type index, registration response values, and decode failures. It exits with an error when any datagram or message does not decode. It also decodes the server messages of the M4 types and the client input (type 1589) with `nw-messages`, encodes them again, and counts those that give the same bytes; a difference is a failure. Inside each replication bundle it does the same for the members at the front of the payload whose class has a reader (types 10, 11, 13 and 15; type 11 is kept as its bytes and its position). The facts it checks are in `docs/re/2026-09-02-carrier-captures.md`.

`content-tool combat-check` checks the combat data of `sim` (M6-A spec section 11.2) without the install: every vitals spawn of a level (default `newworld_vitaeeterna`) must find its mob type and level rows, and the mob max health of each creature record saved from nwdb.info (`--nwdb-dir`, default `local-data/nwdb.info/records/creature`) is compared with ours. A health matches within a thousandth; a health that matches ours at another level is counted apart. It prints counts and datasheet ids only.

Ledgers hold data of other players. Keep them in git-ignored `local-data/captures/`; never commit them or paste message contents.

```bash
export NW_GAME_DIR="/path/to/steamapps/common/New World"
just capture-check
```

To print class names next to type indexes, add `--names local-data/Aeternum-World-main/Catalog/uuid_to_class.json` (Aeternum-World, AGPL-3.0); the tool joins it to `typeindex.json` in your install by UUID.

`nw-tcp-sniff` records the raw bytes that a client sends over TCP, to learn the transport framing. It listens on `127.0.0.1:33435` (the client's `rep-default-port`) and writes dump files to `local-data/tcp-sniff/` (`--listen` and `--out-dir` change them). See `docs/re/2026-09-06-tcp-sniff-run.md`.

## Login server and launcher

`edge-http` is the HTTPS login server. Players log in with the `launcher` program, by username and password or with Discord (one mode per server, `[auth] mode` in `config/edge.toml`).

The server needs a domain name of your own with two host names that point to the server:

- `login.<domain>`: the launcher API and the channel document that the game downloads first.
- `game.<domain>`: the game routes. Every game request carries the game session key in its path (`/s/<session>/...`).

Get one normal public certificate that covers both names (for example from Let's Encrypt with `certbot` or `lego`). The DNS challenge works also when the names point to a LAN address. Put the PEM files where `[tls]` points. The game always uses port 443, so `https_listen` must use port 443. On Linux, give the program that right once after each build: `sudo setcap 'cap_net_bind_service=+ep' target/debug/edge-http`.

Every server reads its file in `config/` (`--config <file>` picks another). An environment variable can override any key: the program's prefix, then the key, with nested keys joined by `__` (for example `NW_EDGE_AUTH__LOGIN_FAIL_LIMIT=5000`).

| Program | Config file | Prefix |
|---|---|---|
| `edge-http` | `config/edge.toml` | `NW_EDGE_` |
| `rep-gateway` | `config/rep-gateway.toml` | `NW_GATEWAY_` |
| `world-director` | `config/world-director.toml` | `NW_DIRECTOR_` |
| `sim` | `config/sim.toml` | `NW_SIM_` |
| `social` | `config/social.toml` | `NW_SOCIAL_` |
| `nw-testclient` | `config/nw-testclient.toml` | `NW_TESTCLIENT_` |
| `blighted-admin` | `config/blighted-admin.toml` | `BLIGHTED_ADMIN_` |

Secrets come from environment variables, never from the config file:

| Variable | Value |
|---|---|
| `NW_SECRET_SIGNING_KEY` | Entry ticket key: `openssl rand -hex 32` |
| `NW_SECRET_GLOBAL_DATABASE_PASSWORD` | Password of `[global_database]` |
| `NW_SECRET_WORLD_1_DATABASE_PASSWORD` | Password of the first world database (the name is set by `database_password_env`) |
| `NW_SECRET_DISCORD_CLIENT_SECRET` | Discord client secret, Discord mode only |

```bash
cargo run -p edge-http -- migrate   # creates missing databases and applies migrations
cargo run -p edge-http -- serve
cargo run -p launcher
```

`edge-http aws-mirror-certificate` makes the self-signed certificate of the `[aws_mirror]` hosts in `config/edge.toml` once. It never replaces a file. The launcher maps those Amazon host names to this server for the game process only (see [docs/client-patch.md](docs/client-patch.md)).

For a release build, `setcap` takes `target/release/edge-http`. Run `setcap` again after each build, because a new file loses the right.

In Discord mode, add `https://login.<domain>/launcher/discord/callback` as a redirect URL of your Discord application.

`edge-http` logs every request that no route serves (`request for an unknown route`, with the session key masked). These lines show what the client asks for that the server does not answer yet.

### M2 exit test (manual)

Do this once in password mode and once in Discord mode (`[auth] mode`).

1. Start the services (`just infra-up`), set the secrets, then run `edge-http migrate` and `edge-http serve`.
2. Start Steam. Start the launcher, type your domain, and log in (or make an account).
3. Click Play. The game starts through Steam with `-ChannelService https://login.<domain>`, `-LoginChannelOverride <game session key>` and `-AuthBackend nw`. The last one makes the game call our `/steam` route (the patched client sends an empty ticket, so Steam may be offline); in `Game.log`, `Persona failed to initialize!` or `Omni metadata is NOT present` means the game took another auth path.
4. The game must reach the character select screen. Make a character.
5. Close the game. Click Play again. The new character must be in the list.
6. Save the `Game.log` lines of the login steps and the `edge-http` log lines of the run in a new note in `docs/re/` as evidence. Remove your account name, Steam ids and IP addresses first.

If the game stops before character select, keep the `Game.log` lines and the `request for an unknown route` lines: they show the next step to build.

## World gateway and test client

`rep-gateway` is the UDP entry point of a world. The game connects to the `rep_address` in `config/edge.toml` after the login queue. The gateway runs DTLS 1.2 and the GridMate carrier, checks the entry ticket in the registration message, answers with a status, and keeps the connection alive with pings. Each ticket works one time only.

`nw-testclient` does what the game does up to that point: it logs in over HTTPS, makes a character when the account has none, waits in the queue, and registers with the gateway.

The game itself cannot connect to the gateway. It trusts only one certificate that is built into the game, and this project does not change that. The test client trusts the gateway's own certificate instead.

Set up the gateway once:

1. In `config/rep-gateway.toml`, set `world_id` to the `[[world]] id` in `config/edge.toml`. The `rep_address` of that `[[world]]` must reach `udp_listen` (the example files match). Each world has its own gateway, so each `[[world]]` has its own `rep_address`.
2. Make the gateway's certificate and key. The files go to `local-data/rep-gateway/` and are never replaced:

```bash
cargo run -p rep-gateway -- generate-certificate
```

Secrets come from environment variables:

| Variable | Value |
|---|---|
| `NW_SECRET_SIGNING_KEY` | The same entry ticket key as `edge-http` |
| `NW_SECRET_TESTCLIENT_PASSWORD` | Password of the test account (`nw-testclient` only) |

```bash
cargo run -p rep-gateway -- serve
cargo run -p nw-testclient
```

`nw-testclient` reads `config/nw-testclient.toml`. Set `login_url` and `game_url` to your domain. The account is made on the first run.

The gateway serves Prometheus metrics on `metrics_listen` (`rep_gateway_*`, among them datagrams and UDP payload bytes in and out, and the player round trip). Keep that address internal.

### M3 exit test

1. `cargo test --workspace` passes.
2. `just capture-check` reads every local ledger with 0 decode errors.
3. Start the services (`just infra-up`), set the secrets, then start `edge-http serve` and `rep-gateway serve`.
4. Run `cargo run -p nw-testclient`. It must print `registration status 0`, then about 60 echoed pings (at least half of one per second of hold) with `closed by server: false`, then a non-zero status for the second registration with the same ticket. The exit code must be 0.

`just test-infra` runs the same walk without the network: the test client logs in through the `edge-http` routes on the Docker PostgreSQL and Valkey, then registers with a gateway on a loopback port.

## World simulation

The open map (`newworld_vitaeeterna`) runs on one or more `sim` processes at 30 ticks per second. `world-director` cuts the map into square cells of `cell_size_m` and gives each live `sim` a stripe of cell columns. Every second it publishes this cell table, and it writes the free player slots of the world to Valkey; the login queue admits players only while there are free slots.

After the gateway checks a ticket, it asks the director to place the character. The director loads the saved position (or, the first time, the next of its `start_positions` in turn, on the ground of the world file), takes a new entity number, and asks the `sim` that owns that cell to take the player. The `sim` sends the login messages. Only then does the gateway send registration status 0.

Every tick, each player gets its own position and the nearest `max_visible_players` players inside `interest_radius_m`, also players of other `sim` processes near a border (ghosts). A client moves by sending its input (type 1589); the `sim` checks the position: finite, inside the level, not faster than `run_speed_mps` x (1 + `speed_margin`) over a budget of `burst_seconds`, and not under the ground or more than `max_height_above_ground_m` above it. A refused move keeps the last good position, which the player's next bundle shows.

A player who walks `handoff_margin_m` into a cell of another `sim` moves to that `sim` (handoff). The player keeps its entity number and does not notice. The gateway sends the player's input to the new `sim` from then on. When a `sim` stops, the director gives its cells to the others; its players stay connected and continue on another `sim` at their last saved position. A `sim` that keeps sending for a connection that the gateway routes to another `sim` for more than 2 s is told to drop that player. Each `sim` saves every player every `save_interval_ms` and when the player leaves; each save carries the time of the player's last accepted move, and the database keeps only the newest.

When no `sim` heartbeat reaches the director for a whole silence window, the director most likely lost its own NATS link: it keeps its table, and the players continue. So a world whose every `sim` stops at once waits until a `sim` comes back.

NATS carries the messages between the gateway, the director and the `sim` processes. NATS has no login in this setup, so keep it on an internal address.

Set up once:

1. Build the world files from your install: `just bundle`, then `just world`, then `just design`. The director reads the ground height for `start_positions`; each `sim` reads it for the move checks.
2. `world_id` in `config/world-director.toml`, `config/sim.toml` and `config/rep-gateway.toml` must be the `[[world]] id` in `config/edge.toml` (the example files match).
3. Run the world database migrations with `edge-http migrate` after an update (M5 adds the entity numbers).

Secrets:

| Variable | Value |
|---|---|
| `NW_SECRET_WORLD_1_DATABASE_PASSWORD` | Password of the world database (the director and `sim` use the variable named by `world_database_password_env`) |
| `NW_SECRET_TESTCLIENT_PASSWORD_B` | Password of the second test account (`nw-testclient pair` only) |
| `NW_SECRET_TESTCLIENT_SWARM_PASSWORD` | Password of every bot account (`nw-testclient swarm` and `hunt` only) |

Start the director and the social service, then the `sim` processes, then the gateway. Each `sim` needs its own `sim_id` and `metrics_listen`; override them with environment variables:

```bash
cargo run -p world-director
NW_SOCIAL_GAME_DIR="$NW_GAME_DIR" cargo run -p social
NW_SIM_SIM_ID=sim-1 NW_SIM_METRICS_LISTEN=127.0.0.1:9102 cargo run -p sim
NW_SIM_SIM_ID=sim-2 NW_SIM_METRICS_LISTEN=127.0.0.1:9104 cargo run -p sim
cargo run -p rep-gateway -- serve
cargo run -p nw-testclient -- pair
```

The director logs the x range of each `sim` (`cells assigned`). The same ids always give the same stripes.

`nw-testclient pair` logs in two accounts (`account` and `second_account`) and keeps both in the world for `hold_seconds`. Each client must see the other. The first walks `route_m` (points east and north of where it stands) at `route_speed_mps`, and the second must see it on the route within `path_tolerance_m`. Then the first sends one move `jump_m` east, which `sim` must refuse. Then the first account logs in again and must appear at its saved position, within `position_tolerance_m`. It prints `pass` or `FAIL` for each check, and the exit code is 0 only when every check passed. `hold_seconds` must be longer than the walk.

`nw-testclient swarm` runs `[swarm] bots` bots (`--bots <n>` sets another count). Each bot takes the entry of `starts_m` nearest to where it appears, walks to its own lane north or south of that start, then east and west across the line through it for `duration_seconds`, on the ground of the world file. Set `starts_m` to the director's `start_positions` and put each on a `sim` border, so every crossing is a handoff. It checks that every bot registered, crossed its line at least `min_crossings` times, stayed on its route, was not closed, got its bundles with no gap above `max_bundle_gap_ms`, and got at least 99 % of its bundles (one per tick). From the metrics of every `sim` in `metrics_urls` it checks the handoffs (enough, none refused or late), that no move was refused, that 99 % of the handoffs and of the ticks took at most one tick (33 ms), and that every metrics page was read. It also prints the PC's UDP receive buffer errors of the step. With `duration_seconds = 0` (also in `[hunt]`), the bots stay until Ctrl-C or until standard input reaches its end, then every bot closes and the report is printed.

For many players on one PC (M5 load spec): give the gateway one `workers` per core you can spare (they share `udp_listen`), keep `udp_buffer_bytes` at 4 MiB (Linux caps it at `net.core.rmem_max`), and set `tick_threads` of each `sim` so that the sims together do not ask for more cores than the PC has.

The bots register their accounts on the first run. `edge-http` counts every registration per IP address against `auth.login_fail_limit`, so raise it for that run, for example `NW_EDGE_AUTH__LOGIN_FAIL_LIMIT=5000`. The gateway counts new connections per IP address too: raise `limits.handshakes_per_ip_per_second` (for example `NW_GATEWAY_LIMITS__HANDSHAKES_PER_IP_PER_SECOND=200`).

The director serves Prometheus metrics on `metrics_listen` (`director_*`), each `sim` on its own (`sim_*`, among them `sim_bundle_members_total`: the members put in bundles). Keep those addresses internal. With `[admin] snapshot_interval_ms` above 0 (at least 100; 0 is off and the committed value), each `sim` publishes a snapshot of its players and awake mobs on `sim.<id>.snapshot` for the admin live panel, and times it in `sim_snapshot_seconds`.

### Social service

`social` runs once per world. It keeps the world's social state (chat, friends, blocks, groups, companies, voice setup), takes the social calls and the presence that the sims send, and sends each sim the messages for its players. It reads `config/social.toml`: `world_id`, `nats_url`, the world database, and `game_dir` (the game install; the profanity lists come from its paks). It serves metrics on `metrics_listen` (`127.0.0.1:9111`).

```bash
NW_SOCIAL_GAME_DIR="$NW_GAME_DIR" cargo run -p social
```

Voice chat needs a Vivox-compatible voice service and the `[voice]` section. Its token key comes from the variable named by `voice.key_env` (`NW_SECRET_VOICE_KEY` in the example). Without that section, the client keeps its voice chat off.

Known gaps:

- `sim` does not send the messages whose layout is still open: types 1628, 3236, 1635 and 6310.
- `sim` reads only the position of the client input. What the other fields of type 1589 mean is mostly Unknown; they are kept as raw bytes.
- The client's reaction to a refused move, and to the jump back of a resumed player, is Unknown: `sim` only sends positions in the player's bundles.
- The speed limit allows sprint with the road boost (6.3 m/s), not mounts or speed effects: `sim` has neither yet.
- How the retail server removes an entity from a client is Unknown. A player who leaves the interest radius is not in the next bundles.
- The retail interest range and limit on visible players are Unknown; `interest_radius_m` and `max_visible_players` are our own choices.
- Cells are shared by count, not by player load. There are no instance `sim` processes yet. `edge-http` gives one `rep_address`.

### M4 exit test

1. `cargo test --workspace` and `just test-infra` pass.
2. `just capture-check` reads every local ledger with 0 decode errors.
3. Start the services (`just infra-up`), set the secrets, then start `edge-http serve`, `world-director`, `sim`, and `rep-gateway serve`.
4. Run `cargo run -p nw-testclient -- pair`. Every check must print `pass`, and the exit code must be 0.

`just test-infra` runs the same pair check without the network, on the Docker PostgreSQL, Valkey and NATS, with a level without height data and two `sim` processes; the walk crosses their border and back. It also writes a saved position to the database and checks that the next login appears there, and it stops one `sim` while a client is on it. On the real level, the walk of `pair` must stay inside the ground band: keep `route_m` short and on flat ground near the start position.

### M5 load test

1. Start `edge-http serve`, `world-director` with `cell_size_m = 1024` and a list of `start_positions` on the `sim` borders, eight `sim` processes, and `rep-gateway serve` with several `workers`, all release builds.
   The gateway refuses connections above `limits.max_connections` (2,000 in the committed config). Raise it above the largest step, for example `NW_GATEWAY_LIMITS__MAX_CONNECTIONS=12000`.
2. Run `nw-testclient swarm --bots 1000`, then 2,500, 5,000 and 10,000, with `mpstat 5` beside it. A step passes when every check prints `pass`. A step that fails while the PC has less than 5 % idle CPU shows the limit of the PC.

### M5 exit test

1. `cargo test --workspace` and `just test-infra` pass.
2. Start the services, set the secrets, then start `edge-http serve`, `world-director`, four `sim` processes, and `rep-gateway serve` on one PC, with the director's `start_positions` on a `sim` border.
3. `cargo run -p nw-testclient -- pair` passes every check.
4. `cargo run -p nw-testclient -- swarm` with 1,000 bots and the `metrics_urls` of the four `sim` processes passes every check.

## Mobs and combat (M6-A, M6-B)

Each `sim` loads the content bundle at start (`content_dir`, the pinned build), keeps only the combat rows, and spawns the vitals spawns of the world file that lie in its cells. Each mob belongs to the `sim` of its spawn. A mob sleeps until a player comes within `wake_radius_m` (default: `interest_radius_m`). Then it wanders, chases a player in sight, attacks in reach, walks back to its spawn when its target is out of the leash circle (and heals there), and respawns after `mob_respawn_ms` when it dies. Mob health and damage come from `VitalsLevelData` (by level) times the mob type's mods and the category mods of its creature type (`VitalsModifierData`).

Players have health (the `Player` row of `VitalsBaseData`). A mob that kills a player makes it dead: it cannot move or attack, and the character is saved at once. When its client sends a respawn request (type 1938) at least `min_respawn_ms` after the death, it lives again with full health where it died. A handoff keeps the player's health. Awake mobs near a border are ghosts in the next `sim`; a hit on a ghost goes to the owner `sim` as a bus message.

Combat on the wire (M6-B): a mob that comes into a player's view is sent once with its spawn card (type 10) and vitals block (type 15), then its position. An input record that sets an attack bit (9, 10, 13 or 15) the record before it did not set attacks the nearest living mob in reach with the first weapon attack. The attacker gets a 2071 for each hit it makes, a player hit by a mob gets a 3601, and a killed player gets a 4299. The numbers marked Unknown in `config/sim.toml` section `[combat]` are not in the game files or the captures.

Checks:

- `content-tool combat-check` (local data, see Capture check): every spawn finds its rows, and our mob health is compared with nwdb.info.
- The fight stress test: `cargo test --release -p sim fight_stress -- --ignored --nocapture` (1,000 players each fighting one mob in one `sim`; tick p99 under 33 ms).
- `nw-testclient chat` logs in the first account (`account`), waits until it knows its own entity, then says one line in area chat (`--line`, default `!help`) and stays for `hold_seconds`. It checks that the session registered, that the line went out, and that an answer came back after it: system chat lines (a GM command of an account with the `game_master` rank), or the line itself as area chat (any other line, through `social`). It prints each system line.
- `nw-testclient hunt` runs `[hunt] bots` bots (`--bots <n>` sets another count) with the accounts, walking speed and ground of `[swarm]`. Each bot walks to the nearest mob it sees (with none in view, to its own point on an 8 m ring around entry `n` modulo the count of `starts_m`; at most 30 bots per start, because each player sees its 47 nearest entities, bots and mobs together), stops within `reach_m`, and attacks every `attack_interval_ms`. After a 4299 it waits `respawn_wait_ms`, sends a 1938, and steps 1 m to show that it lives again. It checks that every bot registered, was not closed, hit a mob (a 2071), lived again after each request, and got its bundles with no gap above `max_bundle_gap_ms`; from the metrics of every `sim` in `metrics_urls`, that at least one mob died per 10 bots, that no move of a living player was refused (a move that arrives after a death, before the bot has its 4299, is counted apart), that 99 % of the ticks took at most one tick, and that every metrics page was read.

## Admin tools

### World map tiles

`content-tool map-tiles` reads the world map tiles of a level from your install (`lyshineui/worldtiles/<level>/map_l{L}_y{Y}_x{X}.dds`, default level `newworld_vitaeeterna`), decodes them (BC1 and BC3 in the pinned client), and writes `local-data/content/map/<level>/<L>/<row>_<column>.png` with an index `tiles.json`. Level 1 tiles are 256 m; each level above halves the grid (8 levels). A tile row counts from the south edge (`--y-index south-up`, the default): joined that way, the tiles of one level form the whole continent. The Oodle decoder cannot read 2 of the 3,031 tiles of the pinned client; the tool lists them and leaves them out. The tiles are game data: they stay in git-ignored `local-data/`, and `scripts/no-game-data.sh` refuses `*.dds` files.

```bash
export NW_GAME_DIR="/path/to/steamapps/common/New World"
just map-tiles
```

### blighted-admin

`blighted-admin serve` serves the admin pages on `http://127.0.0.1:9180/` (`config/blighted-admin.toml`, overrides `BLIGHTED_ADMIN_<KEY>`, nested keys joined by `__`). It reads the world file and the content bundle from `just world` and `just bundle`, and the map tiles from `just map-tiles` when they exist; without them the map shows only the height relief. It listens on a loopback address only, because the tools have no login. A request other than `GET` needs the header `X-Blighted-Admin: 1` and, when the browser sends an `Origin`, the origin of the admin pages; so another web site in your browser cannot use it.

```bash
cargo run --release -p blighted-admin -- serve
```

For the capture viewer it lists every `ledger.bin` under `local-data/captures/` with the name of its `metadata.json` and, for an open-world session, the title of the saved Discord post that holds its archive. Message type names come from the install's `typeindex.json` joined with the Aeternum-World names (set `NW_GAME_DIR`), then from the local Azoth catalog archive (`local-data/azoth-catalog/types.json`, saved by its `archive.py` from their public API).

The capture viewer page (`/web/captures.html`) opens a session and shows its totals, the message types (with names, Azoth catalog status, counts and rates; a click draws the count per second), the entities with their paths on the map, the player's own path, a time slider, the messages near the slider time, the bytes of a message with its bundle members marked, the member classes where the bundle walks stop (the readers still missing), and a type rate comparison with a second session. Capture data stays on this PC: the pages run on a loopback address and keep nothing.

`nw-testclient --ledger <file> <command>` writes the plain carrier datagrams of its first client (received after DTLS decryption, sent before DTLS encryption) as a capture ledger. Put it under `local-data/captures/` and the capture viewer opens it like a recorded session, so a session with our servers can be compared with one of the real game. The ledger holds the login token of the test account's registration: keep it in `local-data/`. `.gitignore` ignores every `ledger.bin`, and `scripts/no-game-data.sh` refuses any tracked file named `ledger.bin` or starting with `NWDL`.

The world map page (`/web/map.html`) shows the map tiles, the relief, or the relief over the tiles at half strength (the check that both line up), and, closer than 4 m per pixel, the mob spawns, gatherables and landmarks of the world file. The page code is plain JavaScript modules built into the program; `just test-web` runs their tests with Node's built-in test runner.

### Live panel

The live page (`/live`) shows the running servers: tiles (sims, players, bots, CPU per program, tick time per sim, packets and bandwidth of the gateway, bundle members, awake and asleep mobs), the map with the sim cells, the players and the awake mobs, the player list, the selected entity, the event log, and buttons that start bot batches of 1, 10, 100 or 1,000 (`nw-testclient swarm` until stopped, one batch after another) and stop them. It reads the bus (`[live] nats_url`, the heartbeats, the cell table and the sim snapshots), the metrics pages in `[live] metrics_urls`, and `/proc` for CPU (Linux only). Bot batches take the `NW_SECRET_*` lines of `[live] testclient_env_file`; the page sends only a size and a batch number. A sim publishes snapshots only with `[admin] snapshot_interval_ms` above 0.

`just live <sims>` starts a whole local test world for it (edge-http on port 8443, the director, the social service, 1 to 8 sims with snapshots every 250 ms, the gateway) and then the panel; Ctrl-C stops all of them. The default is 4 sims. Before you start it:

1. Run `cargo build --release`. The script runs the programs in `target/release/`.
2. Set `NW_GAME_DIR`.
3. Make the three git-ignored local files. The script stops and names each file that is missing:
   - `local-data/m3-test.env`: the `NW_SECRET_*` lines that the servers get.
   - `local-data/edge-http/edge.toml`: the edge-http config of the test world.
   - `local-data/world-director.toml`: the director config of the test world.

Logs go to `local-data/logs/live/`. `LIVE_STACK_SNAPSHOT_MS=0 just live 8` runs it with snapshots off, to compare tick times. The exit test is in `docs/re/2026-09-06-admin-live-panel.md`.

### Studio

The studio page (`/studio`) shows the team's design files in `content/` (see [content/README.md](content/README.md)) with their problems. **Check** runs the full check against the retail files and writes nothing. **Apply** checks, writes the merged bundle and world files into `content_dir` (as `just design` does), then restarts the world-director and the sims through the control FIFO of `just live` (`local-data/logs/live/control`, line `restart-world`) and waits until every sim sends a heartbeat with a new start time (`[studio] restart_timeout_s`, 600 s: a spinning disk with a cold cache needs minutes). A design problem stops Apply before anything is written. Without `just live` (no FIFO), Apply writes the files and tells you to restart the sims yourself. Players on the local world are disconnected by the restart.

To find the design key of a mob spawn, turn on "Mob spawns" on the Map page and click a spawn.

The **Editors** on the studio page write one design file each (through `/api/studio/file`, which runs the schema check first and refuses a file with a problem): an AI profile form, and a map for patrol paths and spawn groups (right-click adds a point; its height comes from the terrain; **Go** moves the map to an x, y). Click a row of the design file list to open it again.

The records page (`/records`) searches the client records of the table kinds (items, weapon, armor and consumable stats, perks, loot tables, mobs, NPCs, quests and quest tasks) by id or English name. A record shows every field with its client value; **Save** writes an `override` file with only the fields you changed, and **Save as new record** writes an `add` file with `base` set to the open record (the item creator). Apply on the studio page to use the files.

On a loot table the records page also has the **drop simulator**: it rolls the table up to 100,000 times with the server's own roll code over the client data and the design files (nothing is written) and shows each item's chance; give the mob's loot tags (`Common`, ...) for tables with a tag condition. **Mobs with no loot** lists the mob types without a loot table. A mob killed by a player rolls its table, and the items go into the killer's bag (a loot bag on the ground is not built yet).

The script editor (`/scripts`) builds visual block scripts for the admin's level: pick blocks (triggers, conditions, actions, `wait`), drag them, click an output dot and then a block to link them, set each block's values, and Save. Apply runs them on the sims; the run log under the canvas shows each block a script passes. Events (waves of mobs, see [content/README.md](content/README.md)) are started and stopped by a game master on the live page under "GM events", at the point picked with a right-click on the map.

The to-do page (`/todo`) keeps the team's tasks in `content/todo.toml`: add a task, or import a GitHub issue (a one-time copy; set `[studio] github_token_env` for a private repository), mark who took it and when it is done, and link the design files it touches. "Changed design files" lists what differs from the last commit, to link before you commit.

Quests: a game master starts any objective for the selected player with **Start quest** on the live page. A player runs one quest at a time; a finished quest gives its item reward (`ItemRewardName`, `ItemRewardQty`) and starts again only when it is repeatable. The world database keeps the active run and the finished quests (`character_quest`). On the records page, an objective shows its task tree.

Items are saved: the world database keeps every character's items (`character_item`), the director sends them with each join, and a sim saves them when they change and when the player leaves. A character that never saved items gets its starter kit; the tutorial always starts from the kit and saves nothing. Run `edge-http migrate` after this update (items and quests are new world database tables).

### Server mode, ranks and GM commands

By default (`[access] mode = "local"`) the admin site listens on a loopback address and has no login: the user on this PC has every right. For the community server, set `mode = "server"` with `hosts` (the names the site is reached by), `global_database` and `global_database_password_env` (the game's account database) and `tls` (certificate and key). The site then serves HTTPS only, and refuses to start when a TLS file is missing. Run `edge-http migrate` after an update: the ranks and the audit log are tables of the global database.

Users log in with their game account and need an admin rank: `csr`, `game_master` or `head_admin`. Give or remove one from the server shell:

```bash
blighted-admin grant-rank ada game_master
blighted-admin revoke-rank ada
```

Five wrong passwords in 10 minutes lock a name for 15 minutes. The session cookie is `HttpOnly`, `Secure` and `SameSite=Strict`, and lasts 12 hours. In server mode the design routes (Check, Apply) and bot batches are refused: design work happens on a team member's own PC and reaches the server through git.

GM commands go to the sim that owns the target, found from the sim snapshots and the cell table, and wait up to 5 s for its answer. Every command (and every login) writes a row to the audit log. On the live page, select a player or bot for: **Teleport** to a point or to another player, **Heal**, **Kill**, **God on/off**, **Kick**, **Give/Take** an item, **Set level** and **Give XP**, **Weapon mastery** (set the rank of one weapon or of all, or give one weapon mastery experience; levels and ranks are the ones the game shows), and **Set coins**. Experience and coins are saved with the progression (world migration 0006), weapon masteries too (0007), and the attribute and weapon points a player spends in the game (0008). The **GM mobs** form spawns up to 50 mobs of a vitals id at a point (right-click the map to fill x and y); they stay awake and respawn until a GM despawns them. The GM page (`/gm`) shows the audit log (a head admin sees all rows, others their own), bans and unbans by login name, and, for head admins, the ranks. Kick needs the CSR rank, rank changes the head admin rank, everything else the game master rank. The admin tools have no mute command yet.

**GM mode in the game client.** An account with the `game_master` rank (or higher) gets the client's own GM mode at login: `world-director` reads the rank from the `[global_database]` of `config/world-director.toml` (leave the section out to give no account GM rights), and `rep-gateway` sets flag byte 1 of the registration response. With `r_DisplayInfo 1` the client shows "GM Client Enabled". The sim then takes the client's GM calls (set level, set coins, change or set a weapon mastery rank, debug teleport) from that account only, and logs and refuses them from every other account. See `docs/re/2026-09-18-progression-and-gm-mode.md`.

**GM chat commands.** The client has no menu or console command for its GM calls, so a game master types commands in chat instead. A line that starts with `!`, on any channel, from an account with the `game_master` rank (or higher) goes to the sim and never reaches the other players. The sim runs it as the admin action of the same name and answers in the System channel, seen only by the sender. From an account without the rank, the line is ordinary chat. A command acts on the sender, or on the player named by one more word at the end (only a player on the same sim). Levels and ranks are the ones the game shows; coins are the raw number, as on the live page. Mastery ids are `CategoricalProgressionData` ids such as `GreatSwordAbilityTable`.

| Command | What it does |
|---|---|
| `!help` | Lists the commands |
| `!where [player]` | Says x, y and z |
| `!level <level> [player]` | Sets the level |
| `!xp <amount> [player]` | Gives experience |
| `!coins <amount> [player]` | Sets the coins |
| `!mastery <mastery id\|all> <rank> [player]` | Sets the rank of one weapon mastery, or of all of them |
| `!masteryxp <mastery id> <amount> [player]` | Gives weapon mastery experience |
| `!heal [player]` / `!kill [player]` | Fills or takes all health (heal also revives) |
| `!god <on\|off> [player]` | Turns damage off or on |
| `!kick <player>` | Saves the player and closes its connection |
| `!give <item id> <quantity> [player]` / `!take <item id> [player]` | Puts items in the bag, or takes every stack of one item |
| `!quest <objective id> [player]` | Starts a quest |
| `!tp <x> <y> <z> [player]` / `!tp <player>` | Teleports to a point, or the sender to a player |
| `!spawn <vitals id> <level> <count>` | Spawns GM mobs around the sender (at most 50) |
| `!despawn` | Removes every GM mob of the sim |
| `!event <start\|stop> <event id>` / `!events` | Starts an event at the sender, stops one, or lists the running ones |

Chat commands skip the admin site's audit log. The sim logs each line and its answer (`GM chat command`, `GM chat answer`).

`LIVE_STACK_INSTANCE=2 just live` starts a second test world next to the first: every port moves up by 100 (admin on 9280), with its own world id and bot names.

## Rules for contributors

- Never commit game files, captures, or `deploy/.env`. Put local game data in `local-data/` (ignored by git).
- Never connect a changed client to Amazon's servers.
- Every new copyleft crate must be added to `[licenses] exceptions` in `deny.toml`.

## Licence and trademarks

### Our code

Blighted Emu is free software. The code written for this project is under the GNU Affero General Public License, version 3 or later (`LICENSE`), unless a crate's `Cargo.toml` names another licence: the launcher and `nw-oodle` are GPL-3.0-or-later, and `nw-formats` is MIT OR Apache-2.0. Third-party libraries keep their own licences, which must be respected; `deny.toml` lists the licences this project accepts. OpenSSL 3 is under the Apache License 2.0, which is compatible with the GPL and AGPL version 3, so no extra linking exception is needed.

### What our licence does not cover

Our licence covers only the code and documents written for this project. It gives no rights to anything that belongs to the owners of New World, including:

- the New World game, its client, its files and data, its art, sound, text and other content;
- the names, logos and trademarks New World, New World: Aeternum, Amazon Games and Amazon;
- any value, table or byte sequence taken from the game or from captures of it, including the small extracts in test files (`testdata/`), which are here only so the tests can check this project against the real game.

Those remain the property of their respective owners and are used here only for interoperability. This repository ships no game files: the tools read game data only from a New World install that you own. Never connect a changed client to Amazon's servers.

### Not affiliated

Blighted Emu is not affiliated with, endorsed by, or connected to Amazon Games or Amazon.com, Inc. New World and New World: Aeternum are trademarks of their respective owners. This is an independent, community-built implementation based on reverse engineering and the work of the New World emulation and preservation community. It is provided for educational and game-preservation purposes only.

---

<sub>This project was moved from a private repository to GitHub. During the move, some of the commit history was changed, so some commit dates and the order of some commits are not exact.</sub>
