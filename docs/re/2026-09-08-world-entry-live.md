# Real client world entry: live results

Date: 2026-09-08. Build: Steam `25810808`, patched `Bin64/BlightedNewWorld.exe`.

This note records live tests of the real New World client entering the
emulator, after the certificate bypass
([cert-bypass plan](2026-09-06-cert-bypass-patch-plan.md)). Facts only; no
capture bytes were copied into the server.

## Summary

The client advances through the post-registration state machine
(`docs/re/2026-08-29-client-protocol-login-gridmate.md`, resolved item 7). Two
walls are cleared; one remains.

| State | Result |
|---|---|
| `WaitingForREPConnection` -> `WaitingForActorGameConnection` | Reached (registration accepted). |
| `WaitingForActorGameConnection` -> `WaitingForSpawnPoint` | **Cleared** by sending type 1628 (`PlayerManagerSelfIdentificationMsg`). |
| `WaitingForSpawnPoint` -> `WaitingForPlayerSpawn` | Reached ("spawn point found"). |
| `WaitingForPlayerSpawn` -> `InGame` | **Not reached.** Still stuck here. |

**Later finding.** The client now gets in game. With the `AlcState::spawn` fix, the world renders
and the player walks ([own player position](2026-09-10-own-player-position.md)).

## What the two fixes did

### Type 1628: PlayerManagerSelfIdentificationMsg

The sim never sent it, so the client could not leave
`WaitingForActorGameConnection`. Its handler `FUN_146454c00` always calls the
`CJavelinActorGame` `ConnectionSuccess` setter (`FUN_145a87010`) at the end, so
the client only needs a body its unmarshal (`FUN_1414fd530`, build `25810808`
`FUN_141516e40`) accepts. The field readers fail only on stream underflow (two
flag bytes must be 0 or 1), and the bulk reader (`FUN_1417ad260`) reads a
prefix-coded count then that many big-endian `u32`, so count 0 reads nothing.

So the smallest accepted body is 157 bytes: a presence block (flag `0x01` + 16
bytes), three `FUN_1415acc30` blocks (`u32` + two 16-byte ids), the
`FUN_1415bad60` block (`u32`, count `0`, two `0` flags, two `u32`), and a
second presence block. All zero except the two `0x01` presence flags.

Live: Game.log logs "actor game connection succeeds" and advances to
`WaitingForSpawnPoint` and then `WaitingForPlayerSpawn`. Committed in `1268829`.

### Type 1635: LevelInfoChangedMsg

Needed so the client loads the level/context. Layout taken from existing data,
not a fresh decompile: the field list of
`local-data/azoth-catalog/packets/1635.json` plus the captured body length
(open world is 106 bytes, from `scratch-bootstrap/layout-profile.txt`). The
published field list sums to 89 bytes; the 17-byte remainder is exactly one
phasing entry. The entry comes **right after the phasing count**, before the
flag bytes (`flagA0`, `clientCtx`, `isMaster`, `flagA3`, `instance`): a first
live try with the entry at the end made the client read `isMaster` as a zero
byte and log "LevelInfoChanged javContext.IsMasterPlayer is false".

Open-world body (big-endian), 106 bytes:

| Field | Type | Value |
|---|---|---|
| map name | vlq32 len + utf8 | `NewWorld_VitaeEterna` |
| coatlicue path | vlq32 len + utf8 | `coatlicue/NewWorld_VitaeEterna` |
| phasing origin X, Y | 2x f32 | 2048, 16 |
| phasing bounds X, Y | 2x f32 | 12272, 10250 |
| reserved | u64 | 0 |
| phasing count | vlq32 | 1 |
| phasing entry | u8 keyType, u32 keyId, u64 phaseId (!=0), u32 extra | 1, 0, 1, 0 |
| flagA0 | u8 | 1 |
| clientCtx | u8 | 1 (every state bundle must carry this) |
| isMaster | u8 | 1 (client drops the message if 0) |
| flagA3 | u8 | 0 |
| instance | u64 | a non-zero clock, must differ from the previous |

Live: Game.log logs "LevelInfoChanged m_clientContextInstanceId 0 changing to
levelInfo.m_clientContextInstanceId 1" (accepted, no "IsMasterPlayer is false").

## The remaining wall: WaitingForPlayerSpawn -> InGame

The client stays in `WaitingForPlayerSpawn` and logs
`GameConnectionWrapper::OnLocalNetworkConnectionWarning` every few seconds. No
`map NewWorld_VitaeEterna` load line and no `InGame` transition appear.

Per resolved item 7, `0xd` -> `0xe` needs all of: `LoadContextAndLevel`
(`FUN_146448cd0`) to have run (it sets `actor_game+0x252` and issues `map %s`,
after both 1628 and 1635); the client context pointer (`FUN_141026080`) to be
non-null; and the local player entity to be spawned (its vtable `+0x30`).

So the open work is the **channel-1 world-state stream**: the client, now on
client context 1, waits for its own player entity in the state bundles
(`ReplicatedStateBundle`, type 8, channel 1). The sim already puts each
player's own member in its bundles (`bins/sim/src/world.rs`), but it is not yet
confirmed that the gateway forwards channel-1 bundles to this real client's
connection, nor that the bundle carries the client-context id (1) the client
now expects, nor that the player's own entity is marked so the client treats it
as the local player. The `0xDDD1000` error after registration is logged once
and is not fatal (the client proceeds past it every time).

Next steps:
1. Confirm the gateway delivers channel-1 bundles to the real client's
   connection (add a per-connection send count, or capture the UDP).
2. Confirm the bundle header carries client-context 1 (candidate fields:
   `byte_3` / `byte_4`, both 1 in captures; see bundle header note).
3. Confirm the player's own entity (matching `ClientAddEntryMsg`'s entity
   number) is in the stream and recognised as the local player.

## Deep dive 2026-09-08 (overnight): why the player does not spawn

Ran more decompiles (read-only, reusing the Ghidra project at
`<re-scratch>/ghidra-25810808`; helper scripts `scripts/StringFuncs.java`
and `scripts/Callers.java`). Build-25810808 addresses.

### The InGame trigger is a state bundle, not a message

`FUN_14644b330` (state machine, old `FUN_14644a070`) case `0xd`: InGame needs
all of `FUN_145a934f0(actor_game)` (`+0x252`, map loaded) != 0, `FUN_141026220()`
(client context) != 0, and its vtable `+0x30` != 0 (local player entity
spawned).

`FUN_146449f90` (LoadContextAndLevel, old `FUN_146448cd0`) is the only thing
that issues `map %s` and sets `+0x252`. It is NOT called by the 1628 or 1635
handlers. Its one caller is `FUN_14643fa70`, the GameMessagePort handler for
incoming replication. When a type-8 `ReplicatedStateBundle` arrives whose
clientContextInstanceId equals the port's expected context and the context
object is still null, `FUN_14643fa70` calls LoadContextAndLevel and logs
"HandleUnexpectedMessage skipping ReceiveStateBundle for bundle message due to
clientContext == nullptr". So the client bootstraps its context/level from the
FIRST matching state bundle. No message on channel 0 does this.

### The bundle's clientContext is the step-3 header byte, and ours matches

`FUN_1406c1240` returns the in-memory bundle byte `+0x30`. The type-8 header
reader `FUN_146b08e40` (old `FUN_146b07870`) writes the wire step-3 byte to
`+0x30` (step 1/2 -> `+0x28`, step 3 -> `+0x30`, step 4 -> `+0x31`, step-5/6
flags -> `+0x8`/`+0x9`, index block -> `+0x38`, payload -> `+0x110`). So
clientContextInstanceId = bundle `byte_3`. The sim sets `byte_3 = 1`
(`OBSERVED_BUNDLE_BYTE_3`) and 1635 sends clientCtx = 1, so they already match.

### The actual blocker: no channel-1 bundle reaches the client

Game.log shows NONE of `FUN_14643fa70`'s log lines (no "skipping
ReceiveStateBundle...", no "MayHandleReplicationUnreliable returning false", no
"received bundle for a previous ClientContext"). So the client's game port is
not receiving any type-8 bundle. With the context byte already matching, the
only explanation is delivery: the real client is not getting the sim's
channel-1 (unreliable) state bundles on the game connection.

Live metrics at the time: `sim_players 0`, `sim_deaths_total{player} 1`,
`rep_gateway_datagrams_out_total 2314`. The player joined with full health
(`world.rs` admit: `Health::full(player_max_health())`) but then was killed by
mobs while idling ~60 s at WaitingForPlayerSpawn (115,522 mobs in the world);
that death is a consequence of not spawning, not the cause.

### Leading hypothesis for the morning

The real client's "actor game connection" (the second channel it starts right
after registration) may be where it expects the state-bundle stream, separate
from how `nw-testclient` (one connection) receives it. The emulator sends
bundles on carrier channel 1 of the one REP connection; confirm whether the
real client reads bundles there or on a distinct actor-game channel/connection.

### Server send path verified (so the blocker is client-side or was masked)

`rep-gateway` peer `send_envelopes` (peer.rs): once `Phase::Registered`, it
sends every channel via `send_now` -> `carrier.send(channel, reliable, ...)`,
including channel-1 unreliable bundles. During ticket checking it holds
reliable messages and drops unreliable ones, but the player had registered, so
channel-1 bundles were being sent. So the gateway delivery path is correct
(the same path `nw-testclient` uses to see positions).

Two things therefore remain for the morning: (a) in the 04:19 run the player
died almost immediately (no spawn protection yet, commit 81a5b09 adds it), so
few/no bundles may have gone out before it left replication; with protection
the player survives and bundles flow every tick, so the first matching-context
bundle should reach the client -> retest first. (b) if the client still gets no
bundle, it is the real client's channel-1 ingestion on its actor game
connection (client-side), which the UDP capture will show.

Morning plan:
1. Retest with spawn protection (81a5b09): the player no longer dies, so
   channel-1 bundles flow the whole time; check Game.log for "skipping
   ReceiveStateBundle" / "map NewWorld_VitaeEterna" / InGame.
2. If still stuck, capture the UDP the gateway sends to the real client
   (`sudo tcpdump -i lo -w /tmp/nw.pcap udp port 33435` during the test) and
   compare channel-1 frames to a retail capture. (or add a per-connection
   channel-1 send counter) to see whether channel-1 bundles are actually sent to
   this connection and in what form.
2. If sent but ignored, RE how the client sets up the actor game connection and
   where it binds the bundle receive (around `FUN_145a905d0` /
   `CJavelinActorGame` and the gridmate channel setup).
3. If not sent, fix the gateway so joined real-client connections get channel-1
   bundles.
4. Separately, stop the idle player being killed by mobs before InGame (spawn
   protection or no mob aggro until spawned) so the death does not confuse the
   next test.

## Morning retest 2026-09-08 (spawn protection build 81a5b09)

Spawn protection works: `sim_players 1` and no death during the wait (the idle
player is no longer killed by mobs). The client still stops at
WaitingForPlayerSpawn and loops `OnLocalNetworkConnectionWarning`, and Game.log
still shows NONE of `FUN_14643fa70`'s bundle-handler lines. Metrics during the
run: `datagrams_out ~2580`, `datagrams_in ~819` (the client is sending us acks/
pings), `sim_players 1`.

So, firmly isolated: the real client ingests channel-0 reliable messages fully
(it processed every login message including 1635) but does NOT ingest our
channel-1 UNRELIABLE state bundles. The gateway sends them (verified in
`send_envelopes`), so this is the carrier framing/handling of unreliable
channel-1 frames. `nw-carrier` has only ever exchanged these with our own
`nw-testclient`, never with the real gridmate client.

A raw UDP capture does not help: the REP traffic is DTLS-encrypted, so tcpdump
would only show ciphertext (and no capture tool is installed anyway). Plaintext
frames are only visible server-side (log them in `nw-carrier`/gateway before
DTLS) or client-side (Frida on the receive path).

### Candidate next steps (pick one)
1. Bisection: send the bundles RELIABLY on channel 1 for one test (reliable
   frames share the framing that channel 0 already works with). If the client
   then ingests them, the bug is specifically our unreliable framing; if not,
   it is channel 1 itself. (sim world.rs ~line 1000 `reliable: false`.)
2. Log the plaintext channel-1 frames the gateway sends (flags/channel/seq) in
   `nw-carrier` and compare to the carrier note's documented frame format
   (docs/re/2026-09-02-carrier-captures.md), looking for an unreliable-frame
   flag/sequence mismatch.
3. Frida-instrument the client's receive path (`FUN_140f89a80` and the frame
   dispatch) to see why it drops channel-1 unreliable frames.

## BREAKTHROUGH 2026-09-08: reliable bundles load the map; member decode is the wall

Bisection result (sim sends bundles reliably on channel 1 via `NW_RELIABLE_BUNDLES`):
the real client DOES ingest reliable channel-1 bundles. Game.log then showed, in
order: "received bundle for a previous ClientContext: expected=0, incoming=1",
LevelInfoChanged to clientContext 1, then **`Loading of map "NewWorld_VitaeEterna"
... allowing map change`** and "skipping ReceiveStateBundle ... clientContext ==
nullptr" (LoadContextAndLevel fired). So:
- CONFIRMED: our `nw-carrier` UNRELIABLE channel-1 framing is wrong for the real
  client (unreliable frames are dropped; reliable ones are accepted). Separate bug
  to fix in nw-carrier.
- The client loaded the open-world map and reached a black screen (map loaded,
  waiting for entities). Two walls past where we were this morning.

Then the real wall, logged every bundle:
```
[ERROR] TypeRegistry: Invalid index requested in GetTypeIdForIndex {"index":13690,"size":7052}
[ERROR] StateReplication: Failure during read of ReplicatedStateBundle. {"stateSequence":...}
```
Our type table has 7052 slots; index 13690 (and 9290) is out of range. This is a DESYNC:
one bundle member's state body is a different length than the real client's reader
expects, so the next member's type-id is read from the wrong offset. The whole
bundle read aborts, so no entity (incl. the player) spawns -> black screen.

ROOT INSIGHT: the replication-and-movement note (2026-09-02) only ever validated
bundles made ENTIRELY of position members (type 13); the capture walk "stops at the
first member that is not type 13". So our MOB-card members (GDE metadata, type 10,
and vitals, type 15, from `bins/sim/src/world/members.rs` `mob_members`) were
reverse-engineered but NEVER validated end-to-end against the real client, and they
are what desync its reader. The player's own member is position-only (validated).

Also seen: "MayHandleReplicationUnreliable() returning false ... seq 0
m_lastUnreliableReplicationSeq 1". The client tracks a per-context replication
sequence and rejects a bundle whose seq is not newer; our bundle sequence numbering
(the group leading / bundles_sent) may also need to match this.

### Debug knobs added (env, off by default, tests green)
- `NW_RELIABLE_BUNDLES` (sim): send bundles reliably on channel 1 (bisection).
- `NW_POSITIONS_ONLY` (sim): omit mob-card members; a bundle then holds only the
  validated position member(s), to test whether the player spawns from those alone.
Both in bins/sim/src/world.rs. The scratch real-client-stack.sh sets both for the
next test.

### Next steps
1. Retest with NW_RELIABLE_BUNDLES + NW_POSITIONS_ONLY: does the player spawn
   (leave WaitingForPlayerSpawn -> InGame) from position-only bundles? If yes, the
   player-entry path works and only the mob-member encoding remains.
2. Fix the mob-card member encodings (GDE metadata, vitals) against a real captured
   type-8 bundle that contains them (byte-for-byte), so the client's reader stays in
   sync. The captures are the ground truth; decode a real non-position member and
   match its body length/layout.
3. Fix the `nw-carrier` UNRELIABLE channel-1 framing so bundles need not be reliable
   (reliable is wrong for the per-tick state flood).
4. Match the bundle replication sequence the client expects
   (m_lastUnreliableReplicationSeq).

## Capture-diff of the mob members (2026-09-08): wrong member composition

Compared our `mob_members` (bins/sim/src/world/members.rs) to the real bundle
member-class frequencies in docs/re/2026-09-02-replication-and-movement.md
("Twenty most common classes"):
- Real mob bundles are dominated by `ALCReplicatedState` (type 11): 4,669,175
  reached member headers, common single-member body 5 or 9 bytes. We do NOT emit
  ALC for mobs.
- We emit `VitalsComponentReplicatedState` (type 15) for every mob, but type 15
  is NOT in the real top-20 member classes at all (the capture walk never reached
  a type-15 member), and docs/re/2026-09-04-m6-vitals-state.md states its wire
  slot layout was never pinned ("the byte offset of current health inside a type
  15 body is not fixed by the static image"). So our type-15 member is an
  unvalidated guess the real client cannot decode -> its reader overruns -> the
  next member's type-id reads as garbage (index 13690) -> whole bundle read
  aborts. This is the desync.
- GDE metadata (type 10): real median body ~281 bytes vs our ~37 (fewer children
  set). This is plausibly fine structurally but also never validated against the client.

Conclusion: the mob-member set/layout we send was invented in M6 and never
validated against the real client. Proper fix = represent mobs with the members
retail actually sends (GDE spawn card + ALC per-tick state), with layouts matched
byte-for-byte to captures; drop the type-15 vitals member from the bundle path.
This is a real RE/impl chunk (finishing the M6 bundle wire format). The player's
own member is position (type 13), which IS validated. So the NW_POSITIONS_ONLY
test should still spawn the player without any mob members.

## Spawn condition traced 2026-09-08 (evening): no local player entity

Live retest after the sequence fix (`65706af`): the client still reaches
`WaitingForPlayerSpawn` and loops `OnLocalNetworkConnectionWarning`.

**Observed (Ghidra, read-only).** The `WaitingForPlayerSpawn -> InGame` poll in
`FUN_14644a070` calls `FUN_141026080`, an `AZ::Interface` getter. Its type
descriptor (`0x14a13c610`) names `.?AVPlayerRegistry@Javelin@@`. The poll needs
`PlayerRegistry` virtual `+0x30` to return non-zero.

**Observed.** `FUN_1468760d0` logs
`PlayerComponentClientFacet::OnEnable(MB::EnableReason /*reason*/)`. When its
own virtual `+0xe8` returns non-zero, it calls `PlayerRegistry` virtual `+0x10`
with its entity (`FUN_14100a7a0` is the same registry getter).

**Inferred.** `+0x10` sets the local player and `+0x30` returns it. The client
sets the local player only when an entity with a `PlayerComponent` client facet
is enabled and that facet reports local.

**Observed.** `FUN_141717fc0` is the per-entity state handler. It starts an
entity portrayal only from GDE metadata (`StartPortrayingGDE`). One branch logs
`Received new local player metadata, updating m_localPlayersServerGDERef.` It
takes that branch when its `interestId` argument is 1 and a check on the entity
object (`+0x850`, virtual `+0x20`) is true.

**Observed (captures, scratch walk, no bytes printed).** In
`NH-MSQ-10.2-Quest` each world change (1635) is followed on channel 1 by a
RELIABLE bundle of about 38 KB, then reliable bundles of about 8.5 KB. The
first member class differs per change (1528, 5606, 3752, 3857), so the
project's member walk stops at once (no reader). The local player's key gets
only type 11 (ALC) members in the reachable stream (never type 13).

**Observed (captures, scratch walk, counts only).** The bundle group integer is
the entity slot (`interestId`); the member's first integer is a component index
(spawn card 0, own-player ALC 16, mob ALC 4 or 5). The own player's ALC is in
group 1 on every bundle of `NH-MSQ-10.2-Quest`. Over 80 sessions, 430 of 465
spawn cards with the player prefab (reached in a bundle's first group) are in
group 1, and 1,322 of about 1,340 bundles above 20 KB start with group 1. The
header index block lists stopped slots (stop count equals list length in every
captured bundle). The player prefab is `slices/player.dynamicslice` (catalog
record size 367,528 bytes, sub id 2); its GUID is on 464 captured cards.

**Corrected.** first-light `info/nw-login-safe-20260502-153840/handoff-notes.md`
names `interest=91` as the local-player record. The counts above put the own
player in slot 1. Its fragment list for that record (3935, 4236, 3147, 4878,
603, 11, 3152, 899, 1528, 10, 3312, 5620, 3183) is still the best list of the
components a full player entity carries.

**Conclusion.** The sim never makes the player's own entity exist on the
client. It sends a type-13 position member for a key the client never
portrayed. Next work:
1. Send the player's own entity as a GDE metadata member (interest 1) with the
   player prefab asset, so the client portrays it and the `PlayerComponent`
   facet enables. The player asset id is Unknown. Candidate source: the game
   asset catalog, not the captures.
2. Learn what the facet virtual `+0xe8` (is-local) checks. Candidate:
   `m_localPlayersServerGDERef` set by the branch above.
3. Learn which members of the 38 KB entry bundle the facet needs
   (`PlayerComponentReplicatedState` 3935 is the candidate).
4. Send the player's own per-tick state as ALC (type 11), as retail does.

## How the live test runs

`scripts/live-stack.sh` forces edge to 8443 for `nw-testclient`. The real
client needs edge on 443, so this test used a scratch variant: the setcap'd
`~/.local/nw-edge/edge-http` with `local-data/edge-http/edge.toml`
(`https_listen = 0.0.0.0:443`), plus `world-director`, `sim`, `rep-gateway` on
udp 33435, all with `NW_GAME_DIR` set and the `m3-test.env` secrets.
`login.<domain>` and `game.<domain>` resolve to 127.0.0.1 in `/etc/hosts`. Launch the
patched client with the launcher's direct-Proton env vars (EAC off). Game.log:
`<prefix>/drive_c/users/steamuser/AppData/Local/AGS/New World/Game.log`.
