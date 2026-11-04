# Blighted New World emulator

A preservation server emulator for New World: Aeternum. The official servers shut down on 2027-01-31.

- Players use their own legal copy of the final PC client. This project never distributes Amazon game files.
- The emulator runs on standard dedicated servers, with no AWS dependency.
- It is non-commercial.

Design: the architecture spec is kept locally (not in git).
Research: [docs/NEW_WORLD_EMULATOR_RESEARCH.md](docs/NEW_WORLD_EMULATOR_RESEARCH.md)
Progress: [docs/progress.html](docs/progress.html) shows what is done, what is missing, and what comes next. Open it in a browser.
Client patch: [docs/client-patch.md](docs/client-patch.md) tells how to make `BlightedNewWorld.exe` (cert bypass) so that the real client can enter the emulator, and how to make it again after a game update.

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

The server reads game rules from your own New World install. Nothing from the game is stored in this repository. Only the table shapes (names and types) are generated into `crates/nw-content/src/generated`.

```bash
export NW_GAME_DIR="/path/to/steamapps/common/New World"
just bundle
just world
just design
just collision
```

`just bundle` writes the retail bundle `local-data/content/retail/bundle-<build>.bin`, then reloads it and checks every table and row. The pinned client build is in `config/client-build.toml`.

`just world` writes the retail world files `local-data/content/retail/world-<build>-<level>.bin` for each of the 41 levels (about 620 MB in all), reloads each file, and prints the record counts, the skipped metadata fields, and the spawn IDs that no datasheet row has.

`just design` reads the retail files, merges the team's design files from `content/` over them (see [content/README.md](content/README.md)), and writes `local-data/content/bundle-<build>.bin`, `local-data/content/world-<build>-<level>.bin` and `local-data/content/design-<build>.json`. The servers and the admin tools read these files. It does not read the game install. With no design files, the merged files are equal to the retail files. When a design file has a problem, it prints each one and writes nothing.

`just collision` writes the level collision files `local-data/content/collision-<build>-<level>.bin` of the tutorial and the open world (about 104 MB in all). A collision file holds the solid shapes of the placed slices of a level, read from the game's physics assets (`.rnr`). It includes the variant capitals as their variation data shows them, and the slices that encounters spawn later. The sims stand the players and mobs on these shapes. A sim does not start without the collision file of its level. See `docs/re/2026-09-20-level-collision.md`.

A sim needs only the bundle, the world file and the collision file of its level. It also reads the files below from `content_dir` when they exist. When one is missing, the sim logs a warning and runs without that part.

| File | Made by | Without it |
|---|---|---|
| `roads-<build>-<level>.bin` | `cargo run --release -p content-tool -- roads [levels]` (default `newworld_vitaeeterna`) | No road sprint boost |
| `water-<build>-<level>.bin` | `cargo run --release -p content-tool -- water [levels]` (default `ftue_v2` and `newworld_vitaeeterna`) | The sea level is the only water |
| `player-attacks-<build>.json` | `cargo run --release -p content-tool -- player-attacks` | Players cannot attack |
| `player-abilities-<build>.json` | `cargo run --release -p content-tool -- player-abilities` | Players cannot use abilities |
| `player-sequences-<build>.json` | No tool. It was mined from capture ledgers. A copy of the table of the pinned build is `bins/sim/src/world/testdata/player-sequences-25810808.json` | The server starts no attack, block, aim, dodge or jump |
| `mob-prefabs-<build>.json` | `cargo run --release -p content-tool -- mob-prefabs [folders]` (reads the ledgers in `local-data/captures` and `local-data/open world captures`) | Mobs are not shown to clients |
| `creature-animations-<build>.json` | `cargo run --release -p content-tool -- creature-animations` (needs the mob prefab table) | Mobs move without animations |
| `ai-data-<build>.bin` | `cargo run --release -p content-tool -- ai-data` (reads `local-data/server-pak/Server.pak`; needs the creature animation table) | Every mob stands at its spawn |
| `ftue-entities-<build>.json` | `cargo run --release -p content-tool -- ftue-entities <ledger.bin>` (a capture of a new character's tutorial) | The tutorial's level entities are not sent |
| `dungeon-scripts-<build>.json` | `cargo run --release -p content-tool -- dungeon-scripts` | Instances run without encounters |
| `camp-slices-<build>.json` | `cargo run --release -p content-tool -- camp-slices` | No camp can be placed |
| `quick-courses-<build>.json` | `cargo run --release -p content-tool -- quick-courses` | No course can be ridden |
| `store-<build>.json` | `cargo run --release -p content-tool -- store` (reads the content bundle; for `edge-http`, `[shop] store_table`) | The game's store sells nothing (the sims build the same table from the bundle themselves) |
| `design-<build>.json` | `just design` | No scripts or events |

The tools that take a bundle read the merged bundle `local-data/content/bundle-<build>.bin`, so run `just design` first. The tools that read ledgers (`mob-prefabs`, `ftue-entities`) use a lot of memory. Run them under a limit, for example `ulimit -v 4000000`. `cargo run -p content-tool -- --help` lists every subcommand.

### Recovery

`content-tool` links `nw-content`, so it cannot run while the generated code does not compile. Removing the `generated` module lines from `crates/nw-content/src/lib.rs` is not enough, because `bundle.rs` and `content-tool build` use `Tables`. Instead:

1. Restore the committed generated files: `git checkout -- crates/nw-content/src/generated`.
2. Fix the cause (usually a change to `bins/content-tool/src/codegen`).
3. Run `just codegen` again.

Codegen stops with an error when a table type name would give an invalid Rust name or a name that the generated code already uses, so a new client build cannot write code that does not compile for that reason.

## Capture check

`content-tool capture-check` reads capture ledgers (`ledger.bin`, decrypted carrier datagrams) with `nw-capture`, decodes them with `nw-carrier` and `nw-reflect`, and prints counts only: datagrams, messages per type index, registration response values, and decode failures. It exits with an error when any datagram or message does not decode. It also decodes the server messages of the M4 types, the client input (type 1589) and the client respawn request (type 1938) with `nw-messages`, encodes them again, and counts those that give the same bytes. A difference is a failure. Inside each replication bundle, it does the same for the members at the front of the payload whose class has a reader in `nw-messages`. The facts it checks are in `docs/re/2026-09-02-carrier-captures.md`.

`content-tool combat-check` checks the combat data of `sim` (M6-A spec section 11.2) without the install. Every vitals spawn of a level (default `newworld_vitaeeterna`) must find its mob type and level rows. The tool compares our mob max health with the max health of each creature record saved from nwdb.info (`--nwdb-dir`, default `local-data/nwdb.info/records/creature`). A health matches within a thousandth. A health that matches ours at another level is counted apart. It prints counts and datasheet ids only.

Ledgers hold data of other players. Keep them in the git-ignored `local-data/captures/`. Never commit them or paste message contents.

```bash
export NW_GAME_DIR="/path/to/steamapps/common/New World"
just capture-check
```

To print class names next to type indexes, add `--names local-data/Aeternum-World-main/Catalog/uuid_to_class.json` (Aeternum-World, AGPL-3.0). The tool joins it to `typeindex.json` in your install by UUID.

`nw-tcp-sniff` records the raw bytes that a client sends over TCP, to learn the transport framing. It listens on `127.0.0.1:33435` (the client's `rep-default-port`) and writes dump files to `local-data/tcp-sniff/` (`--listen` and `--out-dir` change them). See `docs/re/2026-09-06-tcp-sniff-run.md`.

## Login server and launcher

`edge-http` is the HTTPS login server. Players log in with the `launcher` program, by username and password or with Discord (one mode per server, `[auth] mode` in `config/edge.toml`).

The server needs a domain name of your own with two host names that point to the server:

- `login.<domain>`: the launcher API and the channel document that the game downloads first.
- `game.<domain>`: the game routes. Every game request carries the game session key in its path (`/s/<session>/...`).

Get one normal public certificate that covers both names (for example from Let's Encrypt with `certbot` or `lego`). The DNS challenge also works when the names point to a LAN address. Put the PEM files where `[tls]` points. The default ports are 443 (`https_listen`) and 80 (`ping_listen`). On Linux, give the program the right to use them once after each build: `sudo setcap 'cap_net_bind_service=+ep' target/debug/edge-http`. To use other ports, set `public_https_port` and `public_ping_port` in `config/edge.toml` to the ports that players reach. Players then type the HTTPS port after the domain in the launcher (`example.org:8443`), and need a client patched with `--aws-proxy`. See "Servers on other ports" in [docs/client-patch.md](docs/client-patch.md).

Every server reads its file in `config/`, and `--config <file>` picks another. An environment variable can override any key. Its name is the program's prefix, then the key, with nested keys joined by `__` (for example `NW_EDGE_AUTH__LOGIN_FAIL_LIMIT=5000`).

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

`edge-http aws-mirror-certificate` makes the self-signed certificate of the `[aws_mirror]` hosts in `config/edge.toml`. It never replaces a file. When the host list changes, move the two old files away and make the certificate again. The launcher maps those Amazon host names to this server for the game process only (see [docs/client-patch.md](docs/client-patch.md)). The two news hosts serve the files in `config/cms/` (`[cms]` in `config/edge.toml`): the message of the day, the news stories and the menu tiles, and their images. The tracked files have no news. `ping_listen` (port 80, plain HTTP) answers the client's region ping on `dynamodb.us-east-1.amazonaws.com`; the `setcap` right covers it too.

The game's store sells the store mounts of the game data. `[shop]` in `config/edge.toml` turns it on or off (`enabled`) and makes every mount free (`free`, as retail in its last weeks) or priced in Marks of Fortune (`mount_price`, `[shop.mount_prices]`), paid from the account's wallet. `blighted-admin give-marks <login name> <amount>` gives an account Marks of Fortune. `edge-http` serves the catalog, each account's unlocks and its wallet from the store table (`[shop] store_table`); a purchase in the game unlocks the mount for the account (global migration 0005; run `edge-http migrate` after this update). Each store call is logged (`store call`).

Experience is normal by default: 1.0 for every gain, as retail without an event. The `[experience]` section of `config/edge.toml` sets each kind on its own: `character`, `umbral_crystals`, one value per trade skill (`[experience.trade_skills]`), per weapon mastery (`[experience.weapon_masteries]`) and per territory's standing (`[experience.territory_standing]`). A comment above each setting says what it changes. The server sends these values to the game, and the sims use the same values. Restart `edge-http` and the sims after a change.

For a release build, `setcap` takes `target/release/edge-http`. Run `setcap` again after each build, because a new file loses the right.

In Discord mode, add `https://login.<domain>/launcher/discord/callback` as a redirect URL of your Discord application. The server asks Discord for the scopes `identify email`, and saves the Discord username and email of each Discord login (written again at every login) so staff can help a player get an account back. To let in only the members of one Discord server, set `[discord] guild_id` to its server id; the login then also asks for `guilds.members.read` and refuses users outside that server.

To let in only chosen people, set `[discord] allow_list = true` and put their Discord user ids in `allowed_user_ids` (Discord: Developer Mode on, right-click a name, Copy User ID). The list is off by default. While it is on, only those users can log in and make accounts, and a user taken off the list can no longer press Play, even with an older launcher login (like a ban, a game session that is already open runs on until it ends). The server check and the list can be on together. Restart `edge-http` after a change. Every new account is a normal player; staff ranks are given on the admin site.

To keep the characters of a password account when a server moves to Discord mode, run `edge-http link-discord --username <name> --discord-user-id <id>` before the first Discord login (Discord: Settings, Advanced, Developer Mode on; then right-click your name, Copy User ID). Discord login then opens that account with its characters, ranks and account items; nothing is copied. The password login stays, and the admin site still uses it. When that Discord user already made a new account without characters, the login moves over; when that account has characters, the command refuses.

The launcher keeps a list of saved servers (the same on Windows and Linux). Type a domain and click Connect: once it connects, the server is saved under the server's own name. Click a saved server to connect to it. Each saved server keeps its own login, so a switch does not log you out of the other one. "Saved as" renames the selected server; Remove forgets it and its login. The list is in `settings.json` (Settings, "Saved in"); the one server of an older launcher's file becomes the first saved server.

`edge-http` logs every request that no route serves (`request for an unknown route`, with the session key masked). These lines show what the client asks for that the server does not answer yet.

### M2 exit test (manual)

Do this once in password mode and once in Discord mode (`[auth] mode`).

1. Start the services (`just infra-up`), set the secrets, then run `edge-http migrate` and `edge-http serve`.
2. Start Steam. Start the launcher, type your domain, and log in (or make an account).
3. Click Play. The launcher starts the patched game executable directly, never through the Steam launcher. On Linux it runs the game under Proton inside `bwrap`. On Windows it runs the game behind a loopback proxy. The game gets `-ChannelService https://login.<domain>`, `-LoginChannelOverride <game session key>`, `-AuthBackend nw` and `-CognitoPool <id>`. `-AuthBackend nw` makes the game call our `/steam` route. The patched client sends an empty ticket, so Steam may be offline, but a Steam user must be logged in. In `Game.log`, `Persona failed to initialize!` or `Omni metadata is NOT present` means that the game took another auth path.
4. The game must reach the character select screen. Make a character.
5. Close the game. Click Play again. The new character must be in the list.
6. Save the `Game.log` lines of the login steps and the `edge-http` log lines of the run in a new note in `docs/re/` as evidence. Remove your account name, Steam ids and IP addresses first.

If the game stops before character select, keep the `Game.log` lines and the `request for an unknown route` lines. They show the next step to build.

## World gateway and test client

`rep-gateway` is the UDP entry point of a world. The game connects to the `rep_address` in `config/edge.toml` after the login queue. The gateway runs DTLS 1.2 and the GridMate carrier, checks the entry ticket in the registration message, answers with a status, and keeps the connection alive with pings. Each ticket works one time only.

`nw-testclient` does what the game does up to that point. It logs in over HTTPS, makes a character when the account has none, waits in the queue, and registers with the gateway.

The stock game client cannot connect to the gateway. It trusts only one certificate that is built into the game. The patched copy `BlightedNewWorld.exe` does not verify the server certificate, so it can connect (see [docs/client-patch.md](docs/client-patch.md)). The test client trusts the gateway's own certificate instead.

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

`just test-infra` runs the same walk without the network. The test client logs in through the `edge-http` routes on the Docker PostgreSQL and Valkey, then registers with a gateway on a loopback port.

## World simulation

The open map (`newworld_vitaeeterna`) runs on one or more `sim` processes at 30 ticks per second. `world-director` cuts the map into square cells of `cell_size_m` and gives each live `sim` a stripe of cell columns. Every second it publishes this cell table and writes the free player slots of the world to Valkey. The login queue admits players only while there are free slots.

After the gateway checks a ticket, it asks the director to place the character. The director loads the saved position (or, the first time, the next of its `start_positions` in turn, on the ground of the world file), takes a new entity number, and asks the `sim` that owns that cell to take the player. The `sim` sends the login messages. Only then does the gateway send registration status 0.

Every tick, each player gets its own state first, then the players, mobs and ghosts inside `interest_radius_m`. Ghosts are the players of other `sim` processes near a border. A player sees at most `max_visible_players` other players: those that fight it first, then the nearest. Players that are near or in a fight go every tick, and players farther away go less often.

A client moves by sending its input (type 1589). The `sim` checks the position. It must be finite and inside the level. It must not be faster than `run_speed_mps` x (1 + `speed_margin`), which is 13 m/s in the committed config, over a budget of `burst_seconds`. For a rider, the limit grows with the top speed of its mount. It must not be under the ground or more than `max_height_above_ground_m` above it. A refused move keeps the last good position. When the moves of a player stay refused for 1 s, the `sim` moves the player back to its last good position, because the client does not see a refused move.

A player who walks `handoff_margin_m` into a cell of another `sim` moves to that `sim` (handoff). The player keeps its entity number and does not notice. The gateway sends the player's input to the new `sim` from then on. When a `sim` stops, the director gives its cells to the others. Its players stay connected and continue on another `sim` at their last saved position. The gateway tells a `sim` to drop a player when that `sim` keeps sending for the connection for more than 2 s after the gateway routes it to another `sim`. Each `sim` saves every player every `save_interval_ms` and when the player leaves. Each save carries the time of the player's last accepted move, and the database keeps only the newest.

When no `sim` heartbeat reaches the director for a whole silence window, the director most likely lost its own NATS link. It keeps its table, and the players continue. Thus a world whose every `sim` stops at one time waits until a `sim` comes back.

NATS carries the messages between the gateway, the director and the `sim` processes. NATS has no login in this setup, so keep it on an internal address.

Set up once:

1. Build the world files from your install: `just bundle`, then `just world`, then `just design`. The director reads the ground height for `start_positions`. Each `sim` reads it for the move checks.
2. `world_id` in `config/world-director.toml`, `config/sim.toml` and `config/rep-gateway.toml` must be the `[[world]] id` in `config/edge.toml` (the example files match).
3. Run the world database migrations with `edge-http migrate` after an update (M5 adds the entity numbers).

Secrets:

| Variable | Value |
|---|---|
| `NW_SECRET_WORLD_1_DATABASE_PASSWORD` | Password of the world database (the director, `sim` and `social` use the variable named by `world_database_password_env`) |
| `NW_SECRET_GLOBAL_DATABASE_PASSWORD` | Password of the global database (`sim` uses the variable named by `global_database_password_env`; the director uses the one named by `global_database.password_env`) |
| `NW_SECRET_TESTCLIENT_PASSWORD_B` | Password of the second test account (`nw-testclient pair` only) |
| `NW_SECRET_TESTCLIENT_SWARM_PASSWORD` | Password of every bot account (`nw-testclient swarm`, `hunt` and `gather` only) |

Start the director and the social service, then the `sim` processes, then the gateway. Each `sim` needs its own `sim_id` and `metrics_listen`. Override them with environment variables:

```bash
cargo run -p world-director
NW_SOCIAL_GAME_DIR="$NW_GAME_DIR" cargo run -p social
NW_SIM_SIM_ID=sim-1 NW_SIM_METRICS_LISTEN=127.0.0.1:9102 cargo run -p sim
NW_SIM_SIM_ID=sim-2 NW_SIM_METRICS_LISTEN=127.0.0.1:9104 cargo run -p sim
cargo run -p rep-gateway -- serve
cargo run -p nw-testclient -- pair
```

The director logs the x range of each `sim` (`cells assigned`). The same ids always give the same stripes.

`nw-testclient pair` logs in two accounts (`account` and `second_account`) and keeps both in the world for `hold_seconds`. Each client must see the other. The first walks `route_m` (points east and north of where it stands) at `route_speed_mps`, and the second must see it on the route within `path_tolerance_m`. Then the first sends one move `jump_m` east, which `sim` must refuse. Then the first account logs in again and must appear at its saved position, within `position_tolerance_m`. It prints `pass` or `FAIL` for each check. The exit code is 0 only when every check passed. `hold_seconds` must be longer than the walk.

`nw-testclient swarm` runs `[swarm] bots` bots (`--bots <n>` sets another count). Each bot takes the entry of `starts_m` nearest to where it appears. It walks to its own lane north or south of that start, then east and west across the line through it for `duration_seconds`, on the ground of the world file. Set `starts_m` to the director's `start_positions` and put each on a `sim` border, so that every crossing is a handoff. Each bot also changes its block every `block_toggle_ms`. The swarm checks that every bot registered, crossed its line at least `min_crossings` times, stayed on its route and was not closed. It checks that every bot got its bundles with no gap above `max_bundle_gap_ms` and got at least 99 % of its bundles (one per tick). It checks that 99 % of the block changes of the other bots were seen within `max_sync_latency_ms`. From the metrics of every `sim` in `metrics_urls`, it checks the handoffs (enough, and none refused or late) and that no move was refused. It also checks that 99 % of the handoffs and of the ticks took at most one tick (33 ms), and that every metrics page was read. It also prints the UDP receive buffer errors of the PC during the step. With `duration_seconds = 0` (also in `[hunt]`), the bots stay until Ctrl-C or until standard input reaches its end. Then every bot closes and the report is printed.

For many players on one PC (M5 load spec), do these steps. Give the gateway one `workers` per core that you can spare (the workers share `udp_listen`). Keep `udp_buffer_bytes` at 4 MiB (Linux caps it at `net.core.rmem_max`). Set `tick_threads` of each `sim` so that the sims together do not ask for more cores than the PC has.

The bots register their accounts on the first run. `edge-http` counts every registration per IP address against `auth.login_fail_limit`, so raise it for that run, for example `NW_EDGE_AUTH__LOGIN_FAIL_LIMIT=5000`. The gateway also counts new connections per IP address. Raise `limits.handshakes_per_ip_per_second`, for example `NW_GATEWAY_LIMITS__HANDSHAKES_PER_IP_PER_SECOND=200`.

The director serves Prometheus metrics on `metrics_listen` (`director_*`). Each `sim` serves them on its own `metrics_listen` (`sim_*`). Among them is `sim_bundle_members_total`, the members put in bundles. Keep those addresses internal. When `[admin] snapshot_interval_ms` is above 0, each `sim` publishes a snapshot of its players and awake mobs on `nw.<world id>.sim.<id>.snapshot` for the admin live panel, and times it in `sim_snapshot_seconds`. The value must be 0 or at least 100. The committed value is 0, which is off.

### Social service

`social` runs once per world. It keeps the social state of the world (chat, friends, blocks, groups, companies, voice setup). It takes the social calls and the presence that the sims send, and it sends each sim the messages for its players. It reads `config/social.toml`: `world_id`, `nats_url`, the world database, and `game_dir`. `game_dir` is the game install, because the profanity lists come from its paks. It serves metrics on `metrics_listen` (`127.0.0.1:9111`).

```bash
NW_SOCIAL_GAME_DIR="$NW_GAME_DIR" cargo run -p social
```

Voice chat needs a Vivox-compatible voice service and the `[voice]` section. Its token key comes from the variable named by `voice.key_env` (`NW_SECRET_VOICE_KEY` in the example). Without that section, the client keeps its voice chat off.

Expeditions, raids and trials run on `sim` processes with `role = "instance"` in `config/sim.toml`. The `[instances]` section of `config/world-director.toml` sets their waits. With `[instances.launch]`, the director starts and stops instance sims itself.

Known gaps:

- The parts of the client input (type 1589) whose meaning is Unknown are kept as raw bytes.
- The client's reaction to the jump back of a resumed player is Unknown.
- How the retail server removes an entity from a client is Unknown. A player who leaves the interest radius is not in the next bundles.
- The retail interest range is Unknown, and `interest_radius_m` is our own choice. `max_visible_players` is our own choice too. In a retail 45-vs-45 war, one client saw 180 other players.
- Cells are shared by count, not by player load. `edge-http` gives one `rep_address` per world.

### M4 exit test

1. `cargo test --workspace` and `just test-infra` pass.
2. `just capture-check` reads every local ledger with 0 decode errors.
3. Start the services (`just infra-up`), set the secrets, then start `edge-http serve`, `world-director`, `sim`, and `rep-gateway serve`.
4. Run `cargo run -p nw-testclient -- pair`. Every check must print `pass`, and the exit code must be 0.

`just test-infra` runs the same pair check without the network, on the Docker PostgreSQL, Valkey and NATS, with a level without height data and two `sim` processes. The walk crosses their border and back. It also writes a saved position to the database and checks that the next login appears there. It also stops one `sim` while a client is on it. On the real level, the walk of `pair` must stay inside the ground band. Keep `route_m` short and on flat ground near the start position.

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

Each `sim` loads the content bundle at start (`content_dir`, the pinned build), keeps only the combat rows and the player content, and spawns the vitals spawns of the world file that lie in its cells. Each mob belongs to the `sim` of its spawn. Its leash keeps it near the spawn, so it never hands off. A mob sleeps until a player comes within `wake_radius_m` (default: `interest_radius_m`). An awake mob runs the behavior tree of its creature slice from the AI data file. Three tutorial mobs have no tree and fight with the paces measured from the retail tutorial. Any other mob with no tree stands at its spawn. An awake mob with no target and no player within the wake radius for `sleep_after_ms` falls asleep again. A mob that dies respawns after `mob_respawn_ms`. Mob health and damage come from `VitalsLevelData` (by level) times the mods of the mob type and the category mods of its creature type (`VitalsModifierData`).

Players have health (the `Player` row of `VitalsBaseData`). A mob that kills a player makes it dead. A dead player cannot move or attack, and the character is saved at once. When its client sends a respawn request (type 1938) at least `min_respawn_ms` after the death, the player lives again with full health where it died. A handoff keeps the player's health. Awake mobs near a border are ghosts in the next `sim`. A hit on a ghost goes to the owner `sim` as a bus message.

Combat on the wire (M6-B): the `sim` sends a mob that comes into the view of a player once with its spawn card (type 10) and vitals block (type 15), then its position. The attack bits of the input records start the basic attacks of the weapon's stance from the player attack table. A press during the combo link of an attack starts the next step of the link, and the hold picks the light, heavy or charged attack. A melee attack sweeps its damage shapes through their time windows against every target. A projectile or a hit scan fires. The attacker gets a 2071 for each hit it makes. A player hit by a mob gets a 3601, a player hit by a mob's melee attack also gets a 2218, and a killed player gets a 4299. The numbers marked Unknown in `config/sim.toml` section `[combat]` are not in the game files or the captures.

Checks:

- `content-tool combat-check` (local data, see "Capture check") checks that every spawn finds its rows, and compares our mob health with nwdb.info.
- The fight stress test is `cargo test --release -p sim fight_stress -- --ignored --nocapture`. It runs 1,000 players in one `sim`, each fighting one mob, and the tick p99 must be under 33 ms.
- `nw-testclient chat` logs in the first account (`account`) and waits until it knows its own entity. Then it says one line in area chat (`--line`, default `!help`) and stays for `hold_seconds`. It checks that the session registered, that the line went out, and that an answer came back after it. For a GM command of an account with the `game_master` rank, the answer is system chat lines. For any other line, the answer is the line itself as area chat, through `social`. It prints each system line.
- `nw-testclient hunt` runs `[hunt] bots` bots (`--bots <n>` sets another count) with their own accounts and with the sockets, walking speed and ground of `[swarm]`. Each bot walks to the nearest mob it sees, stops within `reach_m`, and attacks every `attack_interval_ms`. With no mob in view, bot `n` walks to its own point on an 8 m ring around entry `n` modulo the count of `starts_m`. Use at most 30 bots per start, because each player sees its 47 nearest entities, bots and mobs together. After a 4299, a bot waits `respawn_wait_ms`, sends a 1938, and steps 1 m to show that it lives again. The hunt checks that every bot began near a hunt start, registered, was not closed, hit a mob (a 2071), and lived again after each request. It checks that every bot got its bundles with no gap above `max_bundle_gap_ms`, and that every server message was read. From the metrics of every `sim` in `metrics_urls`, it checks that at least one mob died per 10 bots and that no move of a living player was refused. A move that arrives after a death, before the bot has its 4299, is counted apart. It also checks that 99 % of the ticks took at most one tick, and that every metrics page was read.

## Admin tools

### World map tiles

`content-tool map-tiles` reads the world map tiles of a level from your install (`lyshineui/worldtiles/<level>/map_l{L}_y{Y}_x{X}.dds`, default level `newworld_vitaeeterna`), decodes them (BC1 and BC3 in the pinned client), and writes `local-data/content/map/<level>/<L>/<row>_<column>.png` with an index `tiles.json`. Level 1 tiles are 256 m. Each level above halves the grid (8 levels). A tile row counts from the south edge (`--y-index south-up`, the default). Joined that way, the tiles of one level form the whole continent. The Oodle decoder cannot read 2 of the 3,031 tiles of the pinned client. The tool lists them and leaves them out. The tiles are game data. They stay in the git-ignored `local-data/`, and `scripts/no-game-data.sh` refuses `*.dds` files.

```bash
export NW_GAME_DIR="/path/to/steamapps/common/New World"
just map-tiles
```

### blighted-admin

`blighted-admin serve` serves the admin pages on `http://127.0.0.1:9180/` (`config/blighted-admin.toml`, overrides `BLIGHTED_ADMIN_<KEY>`, nested keys joined by `__`). It reads the world file and the content bundle from `just world` and `just bundle`. It reads the map tiles from `just map-tiles` when they exist. Without them, the map shows only the height relief. In local mode it listens on a loopback address only, because the tools have no login there. A request other than `GET` needs the header `X-Blighted-Admin: 1`. When the browser sends an `Origin`, it must be the origin of the admin pages. Thus another web site in your browser cannot use the tools.

```bash
cargo run --release -p blighted-admin -- serve
```

For the capture viewer, it lists every `ledger.bin` under `local-data/captures/` with the name of its `metadata.json`. For an open-world session, it also shows the title of the saved Discord post that holds its archive. Message type names come from the install's `typeindex.json` joined with the Aeternum-World names (set `NW_GAME_DIR`). Then they come from the local Azoth catalog archive (`local-data/azoth-catalog/types.json`, saved by its `archive.py` from their public API).

The capture viewer page (`/web/captures.html`) opens a session and shows these items:

- The totals of the session.
- The message types, with names, Azoth catalog status, counts and rates. A click on a type draws its count per second.
- The entities with their paths on the map, and the player's own path.
- A time slider, and the messages near the slider time.
- The bytes of a message, with its bundle members marked.
- The member classes where the bundle walks stop (the readers that are still missing).
- A type rate comparison with a second session.

Capture data stays on this PC. The pages run on a loopback address and keep nothing.

`nw-testclient --ledger <file> <command>` writes the plain carrier datagrams of its first client as a capture ledger. Received datagrams are written after DTLS decryption, and sent datagrams before DTLS encryption. Put the ledger under `local-data/captures/`, and the capture viewer opens it like a recorded session. Thus you can compare a session with our servers to a session with the real game. The ledger holds the login token of the registration of the test account, so keep it in `local-data/`. `.gitignore` ignores every `ledger.bin`, and `scripts/no-game-data.sh` refuses any tracked file named `ledger.bin` or that starts with `NWDL`.

The world map page (`/web/map.html`) shows the map tiles, the relief, or the relief over the tiles at half strength (the check that both line up). Closer than 4 m per pixel, it also shows the mob spawns, gatherables and landmarks of the world file. The page code is plain JavaScript modules built into the program. `just test-web` runs their tests with the built-in test runner of Node.

### Live panel

The live page (`/live`) shows the running servers. It has tiles for the sims, players, bots, CPU per program, tick time per sim, packets and bandwidth of the gateway, bundle members, and awake and asleep mobs. It shows the map with the sim cells, the players and the awake mobs. It also shows the player list, the selected entity and the event log. Its buttons start and stop bot batches of 1, 10, 100 or 1,000. A batch is `nw-testclient swarm` until stopped, and the batches start one after another. The page reads the bus (`[live] nats_url`: the heartbeats, the cell table and the sim snapshots), the metrics pages in `[live] metrics_urls`, and `/proc` for CPU (Linux only). Bot batches take the `NW_SECRET_*` lines of `[live] testclient_env_file`. The page sends only a size and a batch number. A sim publishes snapshots only when `[admin] snapshot_interval_ms` is above 0.

`just live <sims>` starts a whole local test world for the panel, then the panel. The test world is edge-http on port 8443, the director, the social service, 1 to 8 sims with snapshots every 250 ms, and the gateway. Ctrl-C stops all of them. The default is 4 sims. Before you start it, do these steps:

1. Run `cargo build --release`. The script runs the programs in `target/release/`.
2. Set `NW_GAME_DIR`.
3. Make the three git-ignored local files. The script stops and names each file, program and variable that is missing:
   - `local-data/m3-test.env`: the `NW_SECRET_*` lines that the servers get.
   - `local-data/edge-http/edge.toml`: the edge-http config of the test world.
   - `local-data/world-director.toml`: the director config of the test world.

Logs go to `local-data/logs/live/`. `LIVE_STACK_SNAPSHOT_MS=0 just live 8` runs it with snapshots off, to compare tick times. `LIVE_STACK_TICK_THREADS` sets the `tick_threads` of each sim (default 4; 0 is every core). The exit test is in `docs/re/2026-09-06-admin-live-panel.md`.

### Studio

The studio page (`/studio`) shows the team's design files in `content/` (see [content/README.md](content/README.md)) with their problems. **Check** runs the full check against the retail files and writes nothing. **Apply** does the check and writes the merged files into `content_dir`, as `just design` does. Then it restarts the world-director, the social service and the sims through the control FIFO of `just live` (`local-data/logs/live/control`, line `restart-world`). It waits until every sim sends a heartbeat with a new start time, for at most `[studio] restart_timeout_s` (600 s), because a spinning disk with a cold cache needs minutes. A design problem stops Apply before anything is written. Without `just live` (no FIFO), Apply writes the files and tells you to restart the sims yourself. The restart disconnects the players on the local world.

To find the design key of a mob spawn, turn on "Mob spawns" on the Map page and click a spawn.

The **Editors** on the studio page write one design file each, through `/api/studio/file`. That route runs the schema check first and refuses a file with a problem. One editor is an AI profile form. The other is a map for patrol paths and spawn groups. On that map, a right-click adds a point, the terrain gives the height of the point, and **Go** moves the map to an x, y. Click a row of the design file list to open the file again.

The records page (`/records`) searches the client records of the table kinds by id or English name. The table kinds are items, weapon, armor and consumable stats, perks, loot tables, mobs, NPCs, quests and quest tasks. A record shows every field with its client value. **Save** writes an `override` file with only the fields that you changed. **Save as new record** writes an `add` file with `base` set to the open record (the item creator). Use Apply on the studio page to use the files.

On a loot table, the records page also has the **drop simulator**. It rolls the table up to 100,000 times with the server's own roll code over the client data and the design files, and shows the chance of each item. It writes nothing. For tables with a tag condition, give the loot tags of the mob (`Common`, ...). **Mobs with no loot** lists the mob types without a loot table. A mob killed by a player rolls its table for the killer and drops a loot bag at its corpse. The killer takes the items from the bag.

The script editor (`/scripts`) builds visual block scripts for the level of the admin tools. Pick blocks (triggers, conditions, actions, `wait`) and drag them. Click an output dot and then a block to link them. Set the values of each block, and click Save. Apply runs the scripts on the sims. The run log under the canvas shows each block that a script passes. Events are waves of mobs (see [content/README.md](content/README.md)). A game master starts and stops them on the live page under "GM events", at the point picked with a right-click on the map.

The to-do page (`/todo`) keeps the team's tasks in `content/todo.toml`. You can add a task, or import a GitHub issue as a one-time copy. For a private repository, set `[studio] github_token_env`. You can mark who took a task and when it is done, and link the design files that it touches. "Changed design files" lists what differs from the last commit, so you can link those files before you commit.

Quests: a game master starts any objective for the selected player with **Start quest** on the live page. A player runs many quests at one time, up to the journal limit of each quest kind. A finished quest gives its item reward (`ItemRewardName`, `ItemRewardQty`). It starts again only when it is repeatable. The world database keeps the active runs and the finished quests (`character_quest`). On the records page, an objective shows its task tree.

Items are saved. The world database keeps the items of every character (`character_item`). The director sends them with each join, and a sim saves them when they change and when the player leaves. A character that never saved items gets its starter kit. The tutorial always starts from the kit and saves nothing. Items and quests are tables of the world database (migrations 0004 and 0005). Run `edge-http migrate` after an update, because updates add world database migrations.

### Server mode, ranks and GM commands

By default (`[access] mode = "local"`), the admin site listens on a loopback address and has no login. The user on this PC has every right. For the community server, set `mode = "server"` with these keys:

- `hosts`: the names that the site is reached by.
- `global_database` and `global_database_password_env`: the account database of the game.
- `tls`: the certificate and the key.

The site then serves HTTPS only. It refuses to start when one of these keys or a TLS file is missing. Run `edge-http migrate` after an update, because the ranks and the audit log are tables of the global database.

Users log in with their game account and need an admin rank: `csr`, `game_master` or `head_admin`. Give or remove a rank from the server shell. Name the account by its login name, or a Discord account by its Discord username or Discord user id:

```bash
blighted-admin grant-rank ada game_master
blighted-admin revoke-rank ada
```

A server in Discord mode adds `[access.discord]` with the `client_id` of the game login's Discord app and `client_secret_env`, the variable with its secret. The login page then shows only **Log in with Discord**: a site has one login method, and name and password login is off. Add `https://<first [access] host>/login/discord/callback` as a redirect of that app. Only a Discord user who has logged in to the game, with an admin rank, gets in. The ban and rank forms of the GM page also take a Discord username or Discord user id.

Five wrong passwords in 10 minutes lock a name for 15 minutes. The session cookie is `HttpOnly`, `Secure` and `SameSite=Strict`, and lasts 12 hours. In server mode, the site refuses the design routes (Check, Apply), the bot batches and the to-do routes. Design work happens on the PC of a team member and reaches the server through git.

#### Admin site on another machine (relay)

The admin site reads the files, the bus and the processes of the game machine, so it runs there. With a relay, the browsers reach it through another machine, and no port opens into the game machine. The site connects out to the relay over TLS, and each side checks the other's certificate. The relay keeps no game data and no password. A site behind a relay serves no capture data. The admin relay spec (`2026-11-04-admin-relay-design.md`) is kept locally.

1. Make the certificates once. Use the name by which the game machine reaches the relay:

   ```bash
   blighted-admin relay-keys --relay-name admin-relay.example.org
   ```

   This writes `relay.pem`, `relay-key.pem`, `agent.pem` and `agent-key.pem` into `local-data/admin-relay/`. It never replaces a file.
2. On the relay machine, put `relay.pem`, `relay-key.pem` and `agent.pem` in `local-data/admin-relay/` and run `blighted-admin relay` (`config/blighted-admin-relay.toml`). `listen` takes the browsers. Without `tls` it serves plain HTTP, so put a TLS proxy in front (the session cookie is `Secure`). `agent_listen` takes the game machine; open it to the game machine only.
3. On the game machine, put `agent.pem`, `agent-key.pem` and `relay.pem` in `local-data/admin-relay/`. In a copy of `config/blighted-admin.toml`, set server mode with `hosts` (the public admin name) and the global database, remove `listen`, and add:

   ```toml
   [access.relay]
   address = "admin-relay.example.org:9191"
   server_name = "admin-relay.example.org"
   relay_certificate_pem_path = "local-data/admin-relay/relay.pem"
   certificate_pem_path = "local-data/admin-relay/agent.pem"
   private_key_pem_path = "local-data/admin-relay/agent-key.pem"
   ```

   Then run `blighted-admin serve --config <that file>`. It connects again 5 s after a lost connection. While it is not connected, the relay answers 503.

GM commands go to the sim that owns the target, which the site finds from the sim snapshots and the cell table. A command waits up to 5 s for the answer of the sim. Every command and every login writes a row to the audit log. On the live page, select a player or a bot for these commands:

- Teleport to a point or to another player.
- Heal, Kill, and God on/off.
- Kick.
- Give/Take an item.
- Set level, and Give XP.
- Weapon mastery. It sets the rank of one weapon or of all weapons, or gives one weapon mastery experience. Levels and ranks are the ones that the game shows.
- Set coins.

Experience and coins are saved with the progression (world migration 0006). Weapon masteries are saved with it too (0007), and so are the attribute and weapon points that a player spends in the game (0008). The **GM mobs** form spawns up to 50 mobs of a vitals id at a point. A right-click on the map fills x and y. GM mobs stay awake and respawn until a GM despawns them. The GM page (`/gm`) shows the audit log. A head admin sees all rows, and other users see their own rows. The GM page also bans and unbans by login name, and shows the ranks to head admins. Kick needs the CSR rank, rank changes need the head admin rank, and all other commands need the game master rank. The admin tools have no mute command yet.

#### GM mode in the game client

An account with the `game_master` rank (or higher) gets the client's own GM mode at login. `world-director` reads the rank from the `[global_database]` of `config/world-director.toml`. Leave that section out to give no account GM rights. `rep-gateway` sets flag byte 1 of the registration response for that account. With `r_DisplayInfo 1`, the client shows "GM Client Enabled". The sim then takes the GM calls of the client (set level, set coins, change or set a weapon mastery rank, debug teleport) from that account only. It logs and refuses them from every other account. See `docs/re/2026-09-18-progression-and-gm-mode.md`.

#### GM chat commands

The client has no menu or console command for its GM calls, so a game master types commands in chat instead. A line that starts with `!`, on any channel, from an account with the `game_master` rank (or higher) goes to the sim and never reaches the other players. The sim runs it as the admin action of the same name. It answers in the System channel, which only the sender sees. From an account without the rank, the line is ordinary chat. A command acts on the sender, or on the player named by one more word at the end. That player must be on the same sim. Levels and ranks are the ones that the game shows. Coins are the raw number, as on the live page. Mastery ids are `CategoricalProgressionData` ids such as `GreatSwordAbilityTable`.

| Command | What it does |
|---|---|
| `!help` | Lists the commands |
| `!where [player]` | Says x, y and z |
| `!level <level> [player]` | Sets the level |
| `!xp <amount> [player]` | Gives experience |
| `!coins <amount> [player]` | Sets the coins |
| `!mastery <progression id\|all> <rank> [player]` | Sets the rank of one weapon mastery, or of all of them |
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

`LIVE_STACK_INSTANCE=2 just live` starts a second test world next to the first. Every port moves up by 100 (the admin site is on 9280). The second world has its own world id, bot names and log folder (`local-data/logs/live-2/`).

## Rules for contributors

See [CONTRIBUTING.md](CONTRIBUTING.md) for the branch and commit rules.

- Never commit game files, captures, or `deploy/.env`. Put local game data in `local-data/` (ignored by git).
- Never connect a changed client to Amazon's servers.
- Every new copyleft crate must be added to `[licenses] exceptions` in `deny.toml`.

## Licence and trademarks

### Our code

Blighted Emu is free software. The code written for this project is under the GNU Affero General Public License, version 3 or later (`LICENSE`), unless the `Cargo.toml` of a crate names another licence. The launcher and `nw-oodle` are GPL-3.0-or-later, and `nw-formats` is MIT OR Apache-2.0. Third-party libraries keep their own licences, which you must respect. `deny.toml` lists the licences that this project accepts. OpenSSL 3 is under the Apache License 2.0, which is compatible with the GPL and AGPL version 3, so no extra linking exception is needed.

### What our licence does not cover

Our licence covers only the code and documents written for this project. It gives no rights to anything that belongs to the owners of New World, including:

- the New World game, its client, its files and data, its art, sound, text and other content;
- the names, logos and trademarks New World, New World: Aeternum, Amazon Games and Amazon;
- any value, table or byte sequence taken from the game or from captures of it, including the small extracts in test files (`testdata/`), which are here only so the tests can check this project against the real game.

Those remain the property of their respective owners and are used here only for interoperability. This repository ships no game files. The tools read game data only from a New World install that you own. Never connect a changed client to Amazon's servers.

### Not affiliated

Blighted Emu is not affiliated with, endorsed by, or connected to Amazon Games or Amazon.com, Inc. New World and New World: Aeternum are trademarks of their respective owners. This is an independent, community-built implementation based on reverse engineering and the work of the New World emulation and preservation community. It is provided for educational and game-preservation purposes only.

---

<sub>This project was moved from a private repository to GitHub. During the move, some of the commit history was changed, so some commit dates and the order of some commits are not exact.</sub>
