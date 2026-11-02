# RE trace 2026-09-29: journal auto-pin switch and target-lock gates

**Status.** Both blockers are solved. Journal auto-pin: commit `c21828b` sends class 5606
(season tracked stats) with `initialized = 1`. `FUN_143dbcdf0` is the season rewards facet's
handler of the TrackedStat-ready event, not a facet activation
(`crates/nw-messages/src/seasons.rs`). Target lock: commit `110ccc0` sends the mob ALC
`dataBits` (child 41 bit 0 = targetable; child 42 = high byte of the flag word at `+0x450`,
mob value `0x13`; `awake_data_bits` in `crates/nw-messages/src/alc.rs`). The "Open" items
below are history.

Static reverse-engineering trace of two open blockers, done from the whole-client Ghidra
sweep. This complements `local-data/findings/2026-09-29-journal-chest-crash.md` (the live/gdb
findings) with the function-level call chains behind the two Unknowns, so neither has to be
re-derived.

## Sources and identity

- **Client binary analysed:** `BlightedNewWorld.exe` (patched client), image base `0x140000000`,
  sha256 `4c1337e0…b0ebdc6`. Opened read-only; nothing in the game was changed.
- **Function names / addresses:** the whole-client Ghidra sweep at `local-data/re-sweep/`
  (`funcs/*.jsonl.gz`, one record per function with `callers`, `callees`, `strings`, decompiled
  `c`). The sweep's `meta.json` says it ran on **`NewWorld.exe`** (the official client), 354,594
  functions, finished 2026-09-16. **Observed:** `BlightedNewWorld.exe` and `NewWorld.exe` are the
  exact same size (179,214,008 bytes), so the patch is an in-place byte edit that moves no code;
  every `FUN_…` address is valid for both exes. **Observed (byte diff, `cmp -l`):** the patch is
  only **108 bytes** in a few small clusters, all at VA `0x145AA0D33` and higher (plus one byte in
  `.rdata`). That looked like an EAC/anti-tamper bypass stub. **None** of those bytes fall inside
  any journal or lock function below: the lock gate `FUN_142c866e0` is `0x142C866E0..0x142C8752B`
  and the season activation `FUN_143dbcdf0` is `0x143DBCDF0..0x143DBCFD9`, both far below the first
  patched byte. So the patched client is **cleared**: it is not the cause of either bug.
  **Later finding.** The patch is not an EAC or anti-tamper stub. `docs/client-patch.md` names
  its parts: the certificate-verify `NOP`, the Exit-to-desktop vtable pointer (the one `.rdata`
  byte), the offline Steam ticket change (`FUN_145aa0d10`, which holds `0x145AA0D33`), and the
  own-movement correction values. The current patch also writes at `0x140FEC21A`, below the
  range above. That address is also outside the lock and journal functions.
- **Ghidra project with the sweep's analysis:** `~/nw-re/ghidra-25810808/` (build
  25810808).
- Labels follow the project rule: **Observed** (seen in the binary/sweep), **Inferred**,
  **Unknown** (not found; not invented).

## 1. Journal auto-pin: the switch and what gates it

### The mechanism (Observed)

The client only syncs the server's tracked-quest list into on-screen pins while a byte switch is
set. The chain that sets it:

```
[season facet activation, via vtable, no static caller]
  → FUN_143dbcdf0                      season rewards client facet activate
      → FUN_143dbcfe0                  loads "SeasonsRewardsSeasonDataTable"
      → sets facet +0x444 = 1          (facet-initialised byte)
      → gate A and gate B (below)
          → FUN_143a58de0(objectives, 1)
                → *(objectives + 0x5b8) = 1     the pin-sync switch
                → runs the pin sync now: for each tracked objective,
                  FUN_143a64690(objectives, entry, …)   (the pin function)
```

`FUN_143a58de0` is the whole point: with its second arg `1` it both sets `+0x5b8` and immediately
pins every tracked objective. With arg `0` it clears the switch and returns. It has exactly two
callers: `FUN_143dbcdf0` (the activation path) and `FUN_143dba7b0` (an "off"/teardown path). Both
of those have **no static caller**. They are reached only through the season facet's vtable, so
Ghidra cannot name who invokes them. That is the whole reason "which client event turns the switch
on" reads as Unknown: it is a virtual dispatch by the component framework.

**Later finding.** `FUN_143dbcdf0` is not a season facet activation. It is the season rewards
facet's handler of the TrackedStat-ready bus event. The tracked stat facet's state handler
`FUN_143ddbd50` sends that event (`FUN_143dcbfa0`) when class 5606 gets `initialized = 1` on
the local player. Commit `c21828b` sends that body, and auto-pin works
(`crates/nw-messages/src/seasons.rs`, `seasons_tracked_stats_initialized`).

### The two gates inside FUN_143dbcdf0 (Observed)

```c
lVar7 = FUN_141003e70(param_1 + 0x540);   // deref the object stored at facet+0x540
lVar7 = *(longlong *)(lVar7 + 0x80);      // GATE A: pointer at +0x80 must be non-null
if (lVar7 != 0) {
    ...
    plVar8 = FUN_141026220();              // an AZ::Interface<T>::Get() singleton (gate-B subsystem)
    pcVar3 = *(code **)(*plVar8 + 0x58);   // its vtable slot +0x58
    cVar5  = (*pcVar3)(plVar8, FUN_1416b00d0(facet));
    if (cVar5 != '\0') {                   // GATE B: that call must return true
        FUN_143a58de0(lVar7, 1);           // ← flips the switch + pins
    }
}
```

- **Gate A:** the object at `facet+0x540` (deref'd by the null-checked `FUN_141003e70`) must hold a
  non-null pointer at `+0x80`. **Inferred:** this is the local player's objectives facet / the
  season data the activation just loaded; if the season body never populates it, gate A fails and
  the switch never flips.
- **Gate B:** `FUN_141026220` is `AZ::Interface<Javelin::PlayerRegistry>::Get()`. **Observed**
  from the RTTI type string `?AVPlayerRegistry@Javelin@@` at the descriptor it uses. Its vtable
  slot `+0x58`, called with the facet, must return true. **Inferred:** a local-player check
  (matches the live findings note "FUN_143a58de0 … for the local player (PlayerRegistry +0x58)").
  So the switch flips only when the season facet activates **while PlayerRegistry already knows the
  local player**. **Inferred hypothesis:** if the season component activates before the local
  player is registered, gate B fails and nothing pins. Then the fix may be about the order the
  server makes the season component arrive/activate relative to local-player registration, not the
  body bytes alone.

### Why the earlier server fixes did not pin (ties to the live findings)

> From `local-data/findings/2026-09-29-journal-chest-crash.md` (the other session's gdb runs):
> timing is not the cause (27 post-in-game copies, no pin); Fix 1 (class 5485 with only
> `IsInitialized=1`) made `FUN_143dbcdf0` never run at all; Fix 2 (retail's full season body) is
> written but not live-tested.

This trace explains it: `FUN_143dbcdf0` is the facet **activation**, not a per-message handler.
Fix 1's thin body did not activate the facet, so the function never ran and the switch stayed off.
Fix 2's full body is the right direction, but passing gate A **and** gate B is a further condition
beyond merely sending the state. The separate replicated-state **handler** is `FUN_143db8ad0`
(called by `FUN_143db8a70`); it reads the datasets at `+0x1060` and `+0x14d0` and the season-id
list `+0x1278`. It does **not** call `FUN_143a58de0`, so receiving the state is not by itself what
pins.

**Later finding.** The trigger was not the 5485 body. `FUN_143dbcdf0` runs on the class 5606
`initialized = 1` event (commit `c21828b`).

### Next step to close it (needs the live client; cannot be done from static alone)

With Fix 2 (full retail body) applied, run gdb on our server and break on:
1. `0x143dbcdf0` entry: does the facet activate at all? (Fix 1: it did not.)
2. Gate A: is `*(FUN_141003e70(facet+0x540)+0x80)` non-null?
3. Gate B: does `[FUN_141026220()+0x58](…)` return non-zero?
The first of those that is false is the exact thing the server body must still satisfy. Debug our
server only, never retail (EAC). Scripts live in
`local-data/scratch-work/crash/`.

## 2. Target lock: the per-target gate chain

`FUN_142c866e0` is the per-candidate lock test (no static caller; the camera-lock scan
`FUN_142c8bbd0` calls it). Gate order as decompiled, with the exact early-return lines (offsets into
the function's pseudocode) so breakpoints land precisely:

| # | Check (Observed) | Returns (rejects target) at |
|---|---|---|
| 1 | target component ptr `this+0x200` present | line 100 |
| 2 | targetable byte `this+0x1f1` set | line 103 |
| 3 | flag `+0x110` and `[+0x208]->vtable[0xc0]()` | lines 106 to 108 |
| 4 | flag `+0x111` and `[+0x208]->vtable[0xc8]()` | lines 110 to 112 |
| 5 | relation filter `FUN_14171b010(self, target, 1)` | line 183 |
| 6 | hostility `FUN_142a9b4a0(...)` | line 246/256 → 261 |
| 7 | transform/position read `[this+0x200]->vtable[0x48]()` | line 398 |
| 8 | line of sight `FUN_142c95000(target, …)` (two rays) | line 434/437 and 450/452 |

The `this+0x200` component (gate 1, re-used at gate 7 for the transform) and the targetable byte
`+0x1f1` (gate 2) are the cheapest things to get wrong from the server side. The relation filter
(gate 5) and line of sight (gate 8) are the two leads already recorded in
`docs/re/2026-09-12-tutorial-quest.md` §15.3.
**Later finding.** Neither lead was the cause. The lock test reads bit 0 of the mob's state flag
word. Our mobs kept the fresh card's `10 10` in ALC children 41 and 42, so no mob was targetable.
Commit `110ccc0` sends retail's `d3 13` (`awake_data_bits` in `crates/nw-messages/src/alc.rs`).

**Unknown:** which gate our server trips. Static analysis gives the order but not the live verdict.
Break on `0x142c866e0` and log which of the return lines above fires while the user presses lock on
a tutorial sailor; the first one reached is the failing gate.
**Later finding.** Solved: the mob `dataBits` gate (commit `110ccc0`).

## 2b. Capture evidence (content-tool `capture-check`, 2026-09-29)

Decoded with `target/release/content-tool capture-check`. Caveat: the tool fully decodes the
**client→server** stream but only a few **server→client** kinds today (the decoder-coverage gap),
so the season-rewards state (class 5485, server→client) is in the **undecoded** mass and cannot be
pulled from these ledgers yet. That is why the switch needed the live gdb route.

Client→server counts (these ARE decoded):

| Capture | Server | `1458` pin | `2709` auto-pin | `2415` S→C (mob targets you) |
|---|---|---|---|---|
| `captures/own/newchar-retail-2026-09-18` | retail | 12 | 62 | 1 |
| `~/AzothCapture/.../fishing-01` | our server | 11 | 128 | 19 |
| `~/AzothCapture/.../other-01` | our server | 0 | 51 | 4 |
| `~/AzothCaptureBlighted/.../tar lock test … 5` | our server (patched client) | 0 | 30 | 3 |
| `local-data/.../run15/movement/caps/run15` | our server (pre-season-fix, Sep 22) | 0 | 30 | 3 (S→C) |
| `local-data/captures/own/menus-linux-2026-09-12` | retail (high-level char, menus) | 0 | 19 | 0 |

New captures decoded 2026-09-29 (second scan of `local-data/captures/own` and the movement set):

- **`run15` (our server, Sep 22, predates the retail-body fix `c84de98`).** The client received
  **exactly one** class-5485 season state (`bundle members … {…5485: 1…}`), toggled the auto-pin
  slider 30× (`2709`), and sent **zero** pins (`1458` absent). Direct capture proof of the no-auto-pin
  state on our server: one thin 5485, switch never flips. Consistent with `+0x5b8` staying off.
- **`menus-linux` (retail, high-level character opening menus/journal).** No class 5485 at all, no
  `1458`, but 19× `2709`. So on retail, opening the journal and toggling the slider does **not** by
  itself emit a pin. The pin follows the season/quest state, not the UI. Confirms the
  slider and journal-open are not the auto-pin trigger (matches §1 and the live findings).

### Lead: gate B is tied to `PlayerManagerSelfIdentification` (type 1628)

- **Observed (decoder).** `capture-check` round-trips **our server's** type-1628 body fine
  (`run15`: `1628: 1` decoded ok) but **aborts** on **retail's** type-1628 in four captures
  (`newchar-linux`, `general-other-01`, `mv-retail`, `mv-linux`: "record 25/26: type 1628: encoded
  body differs from the capture"). So retail's `PlayerManagerSelfIdentification` body carries fields
  our codec does not model; our server's thinner 1628 is all we round-trip. (A new decoder-coverage
  gap next to 5485. The four retail captures cannot be fully counted until 1628 is modelled.)
- **Observed (code).** `crates/nw-messages/src/level_info.rs`: the client leaves
  `WaitingForPlayerSpawn` and enters the world only after **both** `LevelInfoChangedMsg` (1635) **and**
  `PlayerManagerSelfIdentification` (1628) arrive. 1628 is the message that registers the local player.
- **Inferred.** Gate B of the pin switch (§1: `AZ::Interface<Javelin::PlayerRegistry>::Get()`
  `vt[+0x58]` = "is this the local player") depends on the state 1628 sets up. So the open journal
  question "does the season facet activate while PlayerRegistry knows the local player" reduces to
  **the order and body of 1628 vs the 5485 activation**. Next useful static step: compare our emitted
  1628 body (`PlayerManagerSelfIdentificationMsg`) field-by-field against retail's from a capture, to
  see whether ours sets everything PlayerRegistry needs. This is a lead, not a verdict: gate B still
  needs the one live gdb run to confirm which side fails.

- **Observed:** the pin message `1458` reaches our server fine when the client decides to pin
  (fishing-01: 11; other-01: 0). So the client→server pin pipe works; the failure is only that the
  client never *auto-decides* to pin. That matches the `+0x5b8` switch staying off. 1458 alone
  does not separate auto-pin from a hand pin, so 11 in fishing-01 is not proof of auto-pin.
- **Observed:** `2415` is server→client "a mob selected you as its target" (AI targeting the
  player), present in every run. It is **not** the player's middle-mouse lock-on. The lock-on is a
  pure client-side decision and emits **no** server message, so these captures cannot show which
  lock gate fails. That still needs the `FUN_142c866e0` return-path gdb trace.
- **Inferred:** to pull the 5485 trigger timing from a ledger we would need wire-level type-index
  scanning of the server→client stream (the decoder-coverage work), not `capture-check`.

### 2c. 1628 diff (our server vs retail): 1628 is not the auto-pin blocker

Diff done 2026-09-29 from our codec (`crates/nw-messages/src/self_ident.rs`), our send site
(`bins/sim/src/login.rs:140`), the Azoth spec (`local-data/azoth-catalog/packets/1628.json`,
marked solved/verified), and the local-player note (`docs/re/2026-09-10-local-player-check.md`).

Our `PlayerManagerSelfIdentificationMsg::encode` writes the retail body's **exact positional layout**
with the optional parts empty (157 bytes), field for field:

| Retail field (offset) | Retail value | Our value | Client rule |
|---|---|---|---|
| `identifierA.flags` (0) | 5 | **1** | bit 0 set = GUID form; bit 2 "not known", reads no bytes |
| `identifierA` (1, raw16) | real GUID | **the character raw id** | the local-player check reads this (= characterId) |
| `credential` (17, 108 B) | 3×36 records | zeros | "client accepts all zeros here" |
| `extra.value` (125, u32) | 56 | 0 | "0 works" |
| `extra.list.count` (129, vlq) | 3136 | 0 (empty) | "empty list works" |
| `extra.list.item` (131, …) | 3136 items | none | none |
| `debugFlag` (bool) | 0 | 0 | must be 0 (1 crashes the client) |
| `extra.flag` (bool) | 0 | 0 | 0/1 |
| `extra.vectorX/Y` (2×f32) | 0 | 0 | 0 |
| `identifierB.flags` | 5 | **1** | same as A |
| `identifierB` (raw16) | real GUID | zeros | none |

- **Observed.** The only value differences are the two flag bytes (1 vs 5), the zeroed
  `identifierB`, the empty list, and `extra.value` 0 vs 56. Every one of those is documented as
  accepted by the client (Azoth spec: a minimal all-zero body "is enough for the client to carry
  on"). The one field the local-player check reads (`identifierA`, the character id) our server
  sets correctly (`for_entity`). So the body bytes of 1628 are **not** the auto-pin blocker.
- **Observed (`docs/re/2026-09-10-local-player-check.md`).** The PlayerRegistry local-player entry
  (whose `+0x58` check IS pin gate B) is written by `PlayerComponentClientFacet::OnEnable`
  (`FUN_1468773a0`), gated on the **3935 `PlayerComponent`** state carrying slot 2 with `characterId`
  and `playerConnected = true`, **not** on 1628. 1628 only ends `WaitingForActorGameConnection`. Our
  server already sends that 3935 slot (`player_components.rs` `for_local_player`: `characterId = id`,
  `player_connected = Some(true)`) and the player does reach the world.
- **Inferred.** Because the player reaches in-game, `PlayerRegistry +0x30` already returned the local
  player, so pin **gate B (`+0x58`) very likely passes in-game**. That shifts the remaining weight off
  both 1628 and gate B and onto: (1) does the season facet activation `FUN_143dbcdf0` run at all with
  Fix 2's full body (gdb runs 1 to 4 showed it did **not** with the thin body), and (2) gate A (the
  season data pointer at `facet+0x540 → +0x80` being populated). The one live gdb run on Fix 2 (run 5,
  `seasonsync.gdb`) tests exactly these. Confidence: high that 1628 is cleared; medium on "gate B
  passes in-game", because `+0xe8` in OnEnable is a recovered type-id compare, not a confirmed field
  getter (local-player note §1, residual Unknown).
- **Applied (2026-09-29):** our server now sends flag byte `5` for both presence blocks
  (`self_ident.rs` new `PRESENCE_FLAG_SENT = 0x05`, matching the official server), while
  `decode` still checks only bit 0 (`PRESENCE_FLAG_FIXED = 0x01`). This removes the last
  structural difference from retail's 1628. It is cosmetic for parsing (bit 2 reads no bytes on
  the fixed path) and is not expected to change auto-pin. (Our own 1628 already round-tripped in
  `capture-check`; this does not change that, and it does not fix the abort on **retail's** 1628,
  which comes from its 3136-item list, not from the flag.) The 4 `self_ident` unit tests pass.

## 3. What is confirmed vs still open

- **Confirmed (journal):** the on-screen pin is gated by `objectives+0x5b8`, flipped only by
  `FUN_143a58de0(obj,1)`, reached only from the season facet activation `FUN_143dbcdf0` through two
  gates. It is a facet-activation + gate problem, not a timing or slider problem.
- **Cleared (journal, §2c):** type 1628 is not the blocker. Our body matches retail's layout and
  sets the one field the local-player check reads. Gate B (local player) is set by the 3935
  PlayerComponent via OnEnable, which our server already sends, and very likely passes in-game.
- **Open (journal):** whether Fix 2's full season body makes the facet activation `FUN_143dbcdf0`
  run and gate A (`facet+0x540 → +0x80`) populate. One live gdb run (run 5, `seasonsync.gdb`)
  decides it. This is now the single remaining journal question.
  **Later finding.** Solved by commit `c21828b`: class 5606 with `initialized = 1` makes
  `FUN_143dbcdf0` (the TrackedStat-ready handler) run.
- **Confirmed (lock):** the full gate chain and the exact reject points in `FUN_142c866e0`.
- **Open (lock):** which gate our server trips. One live gdb run on a sailor decides it.
  **Later finding.** Solved by commit `110ccc0`: the mob ALC `dataBits` (children 41 and 42).
- **Closed (both):** the patched-vs-official byte diff. The 108 patched bytes are all at
  `0x145AA0D33`+ (an EAC bypass stub), none inside the lock or journal functions. The patch is not
  the cause.
  **Later finding.** The 108 bytes are the certificate, exit, and Steam ticket changes of
  `docs/client-patch.md`, not an EAC stub. The current patch also writes at `0x140FEC21A`.
