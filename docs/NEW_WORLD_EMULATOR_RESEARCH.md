# New World emulator research overview

Research date: 2026-08-29.
Scope: public sources only.
Goal: a New World preservation server that runs on a normal dedicated server, with no AWS dependency.

This file is the summary. The full detail and all source links are in four research files:

| # | File | Topic |
|---|---|---|
| 01 | [research/01-aws-backend-architecture.md](research/01-aws-backend-architecture.md) | How the official backend ran on AWS, and what replaces each AWS part |
| 02 | [research/02-network-protocol-and-login.md](research/02-network-protocol-and-login.md) | Ports, login flow, game protocol, encryption, anti-cheat, existing projects |
| 03 | [research/03-engine-and-game-data.md](research/03-engine-and-game-data.md) | Engine lineage, client file formats, data-mining tools, client data vs server logic |
| 04 | [research/04-shutdown-precedent-and-legal.md](research/04-shutdown-precedent-and-legal.md) | Shutdown facts, other MMO emulators, legal situation |

Conventions: **Unknown** means that we found no public source. "Unconfirmed" means a weak or single source.

---

## 1. Main facts

1. Shutdown date: 2027-01-31, all platforms. Amazon announced it on 2026-01-15 in "The Future of New World: Aeternum - What to Expect" (https://www.newworld.com/en-us/news/articles/the-future-of-new-world-aeternum-what-to-expect). The game was delisted on 2026-01-15. Cash shop sales stopped on 2026-07-20. Amazon said nothing about offline mode, private servers, or a server or source release. [01, 04]
2. Days left to capture live data (from 2026-08-29): about 155. After 2027-01-31, no live traffic, behavior, or server responses can be recorded. [02, 03, 04]
3. Official backend (AWS for Games blog, 2022-04-06, https://aws.amazon.com/blogs/gametech/the-unique-architecture-behind-amazon-games-seamless-mmo-new-world): [01]
   - Each world: 4 public entry servers ("REPs", EC2 c5.2xlarge) in front of a "hub" of 7 EC2 c5.9xlarge simulation servers.
   - A grid divides the map. Each hub server owns 2 grid parts that are not next to each other. Player state moves between servers when the player moves.
   - Instanced content (expeditions) runs on a separate shared pool.
   - Simulation tick: 30 per second. Capacity: 2,500 players and more than 7,000 AI entities per world.
   - Database: DynamoDB (about 800,000 writes every 30 seconds).
   - Microservices: Lambda behind API Gateway (login queue, sessions, characters, guilds, contracts, achievements, chat channels, unique names).
   - Telemetry: Kinesis, Firehose, S3, Athena.
   - Server code is C++.
4. Engine: the engine is "Azoth", an Amazon fork of Lumberyard, and Lumberyard is a fork of CryEngine. The base Lumberyard version is **Unknown**. Community binary scans say the network library ("Javelin") is a fork of Lumberyard GridMate, not O3DE AzNetworking. [02, 03]

## 2. How the client talks to the server

The client uses two network planes. [02]

### 2.1 HTTPS control plane (login and services)

- All requests go to fixed hostnames: CloudFront, API Gateway (five clusters), `tokenservice.amazongames.com`, and an S3 remote-config bucket. The full list is in file 02, section 2.1.2.
- Login sequence:
  1. Load the channel config (`/STEAM_APP_ID.1063730.json`).
  2. Get a Steam session ticket.
  3. Exchange it for an OmniSDK JWT.
  4. Exchange the JWT for temporary AWS credentials. The client signs later calls with SigV4.
  5. Call `getlogininfo` to load the world list and the character list.
  6. Validate or create a character.
  7. Join the login queue.
- The login queue reply holds `RepAddress` (`host:port`). This tells the client which game server to connect to. `Signature` must not be empty.
- Status: a stock client can use an emulated control plane. It needs a hosts-file or DNS redirect, a local certificate authority (CA) installed on the player PC, and a local HTTPS service. Community project first-light finished this step.

### 2.2 Game data plane ("REP")

- Transport: UDP with DTLS 1.2, cipher `ECDHE-RSA-AES256-GCM-SHA384`. first-light saw UDP port 24083.
- Inside DTLS: a GridMate Carrier datagram layer.
  - 4-byte header with a 16-bit sequence number.
  - Mostly big-endian.
  - Channels: 0 = reliable events and RPC, 1 = replication state bundles, 3 = system.
  - Optional LZ4 block compression.
- Above that: AZ-reflected typed messages (`type_idx`, LEB128 varints) and replication bundles. Message type indexes are set at client startup, so they change between builds.
- Status at the research date: blocked. The client trusts only a certificate that is built into `NewWorld.exe`. A local server cannot present that certificate, so the DTLS handshake fails with `unknown_ca`. Easy Anti-Cheat (EAC) refused a client with a changed file on disk. The research files describe this blocker. They do not give bypass steps.
- Status in this repository: the launcher starts a patched copy of the client that does not check the server certificate, and it does not start Easy Anti-Cheat. See [client-patch.md](client-patch.md).

### 2.3 Official port guidance

TCP 80 and 443. UDP 27000-27050 inbound. UDP 20000-59999 outbound. Steam ports. The EAC article also lists 18887-18903; the purpose of that range is **Unknown**. [02]

## 3. Game data in the client

A large part of the game rules is in the client files. [03]

- About 125 `.pak` files. Each is a standard ZIP. Entries use store, deflate, or method `0x0F` (Oodle). No tool we read decrypts them, so they are probably not encrypted (inferred). On Linux, `ooz` (GPL-3.0) decodes Oodle.
- Formats with community parsers: `.datasheet` (binary tables), AZ ObjectStream (`.slice`, `.dynamicslice`, `.slicedata`, often in an AZCS zlib wrapper), `.loc.xml`, `.luac` (2-byte prefix), world JSON files, `region.heightmap` (16-bit TIFF), `region.distribution`, `.actionlist`.
- Data that ships in the client: loot tables and buckets, mob stats (vitals), damage and spell tables, abilities, status effects, perks, recipes, quests, NPCs, game modes, AI action lists, spawner and encounter settings (in `...ServerFacet` slice data). About 2,250 tables.
- Data that does not ship: the simulation code. This includes the damage formula code, the loot roll code, the AI runtime, pathfinding (navmesh source **Unknown**), the quest state machine, wars, invasions, and persistence. We must rebuild all of these.
- O3DE (Apache-2.0 or MIT) uses the same ObjectStream and AZCS formats. It is a safe reference. The archived Lumberyard code has a custom AWS licence. Do not copy it.

Main tools:

| Tool | URL | Licence | Status |
|---|---|---|---|
| nw-buddy (`nwbt` CLI) | https://github.com/giniedp/nw-buddy | MIT | Active. Most complete. |
| new-world-tools | https://github.com/new-world-tools/new-world-tools | MIT | Last push 2025-11-05 |
| ooz | see file 03 | GPL-3.0 | Oodle decoder |

## 4. Existing New World emulator projects

None has reached in-world gameplay. [02, 04]

| Project | URL | Status | Notes |
|---|---|---|---|
| first-light | https://github.com/nw-private-server/first-light | Defunct on GitHub. Work moved to the "OpenWorld" Discord. | HTTPS auth flow done. Blocked at DTLS and V3 registration. Used Frida to bypass pinning and a non-EAC launch path. No licence, so do not copy its code. |
| NewWorldPreservation | https://github.com/AScriver/NewWorldPreservation | Research workspace | Stock client only. Reached local HTTPS. Stopped at `unknown_ca`. |
| Aeternum-World | https://github.com/Coldzer0/Aeternum-World | Analysis tools, AGPL-3.0 | Best public notes on the packet layers. |

## 5. AWS-to-dedicated-server replacement map

From file 01, section 3.1, and file 04, section 5.

| AWS part | Role | Replacement |
|---|---|---|
| EC2 REPs (4) | Public UDP entry | One `rep-gateway` (DTLS/UDP) |
| EC2 hub (7 servers) + grid hand-off | 30 Hz world simulation | One `world` process. Keep the grid cells as data, not processes. Sleep cells with no players. |
| Shared hub pool | Expeditions | Instance manager in `world` |
| Lambda + API Gateway | Login queue, sessions, characters, guilds, contracts, achievements, chat channels, names | One `services` backend with one module for each AWS service name |
| CloudFront + S3 remote config | Channel JSON, remote config, news | `bootstrap-http`: a local HTTPS server that routes by `Host` header |
| Amazon token service / OmniSDK / STS | Login tokens and AWS credentials | `auth`: issues its own JWTs and fake STS credentials |
| DynamoDB | Game state | PostgreSQL. Use one transaction for each value transfer. Keep static content apart from player data. |
| ElastiCache | **Unknown** | Valkey or Redis, or an in-process cache. Optional. |
| Kinesis, Firehose, Athena, Redshift | Telemetry and analytics | Not needed. Accept and discard client telemetry. |
| Elasticsearch / OpenSearch | **Unknown** | PostgreSQL full-text search if a feature needs it |
| EAC | Client integrity | Not available after shutdown. Effect is **Unknown**. |

This repository builds these parts under other names:

- `rep-gateway` is the UDP entry point of the world: DTLS, the GridMate carrier, and the login ticket check.
- `sim` runs one cell of a level at 30 Hz. `world-director` gives the cells to the sims.
- `edge-http` is the HTTPS login server: the launcher API and the game login routes.
- `social` is the social service of one world: chat, friends, blocks, groups, companies and voice setup.
- `nw-db` holds the PostgreSQL migrations and records for accounts, launcher sessions and characters.
- `nw-content` holds the game rule tables and the content bundle file. The game rules come from that file, not from the database.

Design lessons from the 2021 incidents [01]:

1. Make every trade, trading post action, and treasury change atomic for all parties. (The 2021 gold dupe came from a trade that was not saved atomically.)
2. Validate all state on the server. The server is authoritative.
3. Sanitize chat. (In 2021, chat rendered HTML.)
4. Keep point-in-time restore (PostgreSQL WAL archiving).
5. Keep an audit log of all currency and item creation and transfer.

## 6. Lessons from other MMO emulators

Full table in file 04, section 4. [04]

- Closest models:
  - NexusForever (WildStar, C#): separate Auth, STS, and World services. It redirects the client with launch arguments only, so no patch is needed.
  - MHServerEmu (Marvel Heroes, C#): a local TLS reverse proxy and SQLite.
- The pattern that works:
  1. Capture traffic and data before shutdown.
  2. Extract static data from the client.
  3. Split the server into login, world, and database parts.
  4. Redirect the stock client.
  5. Pin one exact client build.
- Return of Reckoning and The Crew Unlimited both depended on captures made before the shutdown. The Crew Unlimited shipped an offline local-server mode first.
- Do not distribute game files. Do not take money. Scapegaming (a paid WoW server) got an $88.6M default judgment.

## 7. Legal situation (facts only, not legal advice)

[03, 04]

- The Amazon Games Terms of Use forbid reverse engineering, data mining, hosting without consent, and "unauthorized connection that emulates portions of the Games".
- US DMCA 1201 exemption (37 C.F.R. 201.40): it covers local play of games whose server support has stopped. It does not cover online services. On 2024-10-25, the Copyright Office refused a remote-access exemption. The 2027 rulemaking cycle is open.
- Bnetd (2005): the court enforced the EULA ban on reverse engineering against a server emulator.
- EU "Stop Destroying Videogames": 1,294,188 verified signatures. On 2026-06-16, the Commission declined to make a law. It proposed a code of conduct.
- City of Heroes Homecoming got a revocable licence from NCSoft in 2024. A licence is possible, but rare.
- Get legal advice before any public release.

## 8. Recommended architecture (from the research)

```
Player PC (stock client, pinned build 1.400.6031.x)
   |  hosts file / local DNS + local CA
   v
Dedicated server
   bootstrap-http   HTTPS 443  channel JSON, remote config, MOTD, news stubs
   auth             HTTPS 443  OmniSDK JWT, STS-shaped credentials
   services         HTTPS 443  getlogininfo, characters, login queue (RepAddress),
                               guilds, contracts, achievements, names, chat channels
   rep-gateway      UDP/DTLS   GridMate Carrier, replication, RPC
   world            in-process 30 Hz simulation, grid cells, instances, AI
   PostgreSQL                  content DB (from datasheets) + player DB
```

This is the plan from the research. Section 5 lists the parts that this repository builds.

## 9. Blockers and open questions

Ranked by impact on the project:

1. REP certificate trust (`unknown_ca`). The stock client cannot connect to a private game server. This needs a project decision and a legal review. [02, 04] This repository now uses a patched client copy (section 2.2).
2. The EAC backend after shutdown. Can the client log in or open REP with no live EAC? **Unknown.** Test before 2027-01-31. [02]
3. The protocol message schemas. They change between builds, so pin one build. In-world captures are needed, and the traffic is encrypted. [02]
4. `@assets@/ClientOverride.json`. The client looks for this file. Can it override endpoints? The format is **Unknown**. [02]
5. Steam ticket verification. It needs a publisher key that we do not have, so the server must accept tickets without verification. [02]
6. Server-only logic (formulas, AI runtime, navmesh). This must be rebuilt and checked against live behavior. [03]
7. Legal position. [03, 04]

## 10. Time-critical actions before 2027-01-31

1. Archive the final client build (all paks, `Bin64/NewWorld.exe`, the Oodle DLL, and the Steam build manifest).
2. Run nw-buddy `nwbt` and new-world-tools on the client. Archive the datasheets, object streams, and type tables with the build number.
3. Record HTTPS login responses and in-world behavior: damage numbers, drop rates, AI behavior, quest flows, NPC dialogue, and UI states (video and logs).
4. Test how the client behaves when EAC and the Amazon endpoints are not reachable.
5. Join or contact the "OpenWorld" Discord (first-light) and the NewWorldPreservation project. This prevents duplicate work.
6. Get a legal review.

## 11. Conflicts between the research files

- File 04 could not confirm the nw-buddy repository URL. File 03 confirmed it through the GitHub API: https://github.com/giniedp/nw-buddy (MIT). File 03 is correct.
- File 03 says it found "no emulator project". Files 02 and 04 list three early projects (see section 4). File 03 searched only for leaks and playable emulators. All four files agree that none is playable.
- first-light calls the Lumberyard licence MIT. The `LICENSE.txt` in the Lumberyard repository shows a custom licence. [02]
