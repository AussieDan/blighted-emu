# Tutorial quest: the server script of the whole tutorial

Date: 2026-09-12. Build: Steam `25810808`. Labels: **Observed**, **Inferred**, **Unknown**,
**Our choice**.

Sources: our own live capture of a new character (warmonger pick) from login to the open world
(`local-data/captures/own/newchar-linux-2026-09-12/`), the content bundle (`ObjectiveTasks`,
`VitalsBaseData`, `BackstoryDefinition`, `MasterItemDefinitions`), and the Azoth packet list
(`local-data/azoth-catalog/packets/`). The full step table with times is in the session notes
(not committed: it holds the live character's ids).

## 1. The tutorial is one objective task tree

**Observed.** The tutorial is the objective `01_SoleSurvivor2`, root task
`01_SoleSurvivor2_Container`. Its `ObjectiveTasks` tree is the server script: every achievement
change of the live run is a `TaskUnlockAchievement` of the tree, and the own objectives state
(type 3857, component 22) shows each task done at the same millisecond as its game event.

**Observed.** The client plays every video, cutscene, hint and door on its own when the server
sets the achievement that unlocks it. The server must:

- walk the tree on events and set its achievements;
- answer the cutscene handshake (6703 → 6707, 6705 → 6709);
- spawn the fights;
- take the tutorial sword, fill the bag, and equip from it;
- move the player into the water.

Task types and what completes them (implemented in `bins/sim/src/objectives.rs`):

| Type | Done when |
|---|---|
| `ConsecutiveTaskContainer` | its children are done one after the other |
| `PartialTaskContainer` | one child is done (the others stop) |
| `SimpleTaskContainer` | all children are done |
| `TaskUnlockAchievement` | at once; sets the achievement |
| `TaskRemoveItem` | at once; removes `TargetQty` of the item (every row has one, 1 to 7), the bag first (**Inferred**); all of it when the player holds no more |
| `TaskTimer` | after `Duration` seconds |
| `TaskCinematicStart` / `TaskCinematicComplete` | 6606 / 6542 with the CRC-32 of the lowercase `ItemName` |
| `TaskCutsceneComplete` | 6705 of the cutscene the client asked for (6703) |
| `TaskTriggerArea` | the player within 12 m of `OverrideLocation` (**Our choice**: retail tasks completed 4 to 12 m from it; the real volumes are level data) |
| `TaskKillContribution` | `TargetQty` kills whose vitals id or `VitalsCategories` names `KillEnemyType` (an id start counts: `Legion_Legionnaire` names `Legion_Legionnaire_FTUE`; **Inferred**) |
| `TaskHaveAndReturnItems` | the player gets `ItemName`; without one, the player takes all items of a loot container (**Observed**, objectives state 3857 of newchar-retail-2026-09-18 and newchar-linux-2026-09-12: `EquipWeapon_partial` and the `MSQ_Tutorial_Inventory` unlock completed 66 and 56 ms after the bag take 2939, in both runs through the three `Take*` tasks of `Weapon2_container`, each with progress 3, the bag's item count; the four of `Weapon1_container` were not listed. Why `Weapon1_container` did not count is **Unknown**: its rows differ from those of `Weapon2_container` in no field that `TaskRow` reads; the server completes the partial through `Weapon1_container`, at the same time) |
| `TaskEquipItem` | 3895 equips `ItemName` (**Observed** later in retail: the equip tasks completed 3.1 to 5.8 s after the weapon equips, and the potion equip task only at the next kill, 13.5 to 24.7 s after; what makes retail check them is **Unknown**) |
| `TaskOpenUiScreen` | 2617 with the CRC-32 of the lowercase `UIScreenTag` (`hud` = `a1126b78`) |

**Observed.** With the retail events of the live run in order, the content's tree sets the 27
achievements of the run in the retail order (test
`the_real_tutorial_quest_runs_to_its_end_on_the_retail_events`, run with `NW_CONTENT_DIR`).

## 2. Client calls

**Observed** (Azoth names, our capture bodies). Header: two big-endian `u64` (target, entity).

| Type | Name | Body after the header |
|---:|---|---|
| 6606 | `PlayerCutsceneComponentServerFacet_NotifyCinematicStarted` | `u32` video id |
| 2617 | `ObjectivesComponentServerFacet_NotifyOpenUiScreen` | `u32` screen id |
| 2939 | `PlayerTradeComponentServerFacet_TakeAllItems` | 16-byte container entity id, `u64` container facet entity, `u64` **Unknown** |
| 3895 | `PaperdollComponentServerFacet_ServerEquipItem` | `u32` bag slot, `u32` paperdoll slot, `u32` quantity, 16-byte item instance id, `u8` (0) |

## 3. Items

**Observed.** The four armor pieces and `1hSwordT2_FTUE` are worn at login (the `InventoryItem`
pairs of quantity 1). At the end of the water cutscene the tree removes the sword. The beach bag
gives the pairs of quantity 0: the archetype's two weapons and helmet. Equipping both weapons
completes the archetype's equip container, which sets `FTUE_AncientRuins_Door`; the client opens
the door.

**Observed.** A replicated list whose first number is 0 is the full form, and the reader takes
it at any time (paperdoll note, section 2). **Our choice:** after a gear change, the server sends
the four gear states again whole, at a higher version, instead of deltas.

**Inferred.** Paperdoll slots 0 to 4 (armor) and 24 to 29 (weapon options) are the visible items;
other slots (amulet 5, quick slots 14 to 17) are the hidden items (retail: the potion stack in slot
14).

**Our choice:** a handoff carries the items (`HandoffOffer.items`), so the open world keeps the
tutorial's gear. There is no item store yet: a new login starts from the kit again.

## 4. Fights and moves

| When | Retail | Here |
|---|---|---|
| Login | sailor 1 waits at (467.62, 846.01, 90.16) | same, personal mob, held |
| 6638 (quick-time lesson done) | sailor 1 fights | same |
| Sailor 1 dead | sailors 2 and 3 at (468.73, 847.22) and (468.73, 845.17) | same |
| 6606 `ftue_intro3` + 13.5 s | own position moves to (793.84, 337.42, 26.16), no loading screen | the next own group carries the full first position there |
| `UnlockDoor` done | legionnaire at (668.27, 385.16, 27.35) (+4 s) | at once |
| `FTUE_First_Fight_Trigger` done | signifier at (621.60, 419.50, 20.99) | at once (**Our choice**: same look as the legionnaire; both have vitals `Legion_Legionnaire_FTUE`) |
| `Combat02_Trigger_Failsafe_ForClimb` done | corruption entity and 3 swarmers near (579.6, 381.9) | same |
| `FTUE_TF_cutsceneComplete` | Thorpe (`Damned_Commander_FTUE`) at (490.25, 209.89, 22.83) | same |
| Thorpe low | dies with 7.6 % health left, no killing hit (**Observed**); the client starts the next cutscene | dies at 10 % (**Our choice**); not a quest kill |
| 502 after the last video | loading screen, open world at (7280.49, 3141.49, 40.12) | handoff to the open-world sim; the exit area no longer ends a tutorial whose quest runs |

**Observed.** The vitals categories: sailor `Corrupted_Sailor_1H_Club`, legionnaire and signifier
`Legion_Legionnaire`, corruption entity and swarmers `Corruption_Entity`; Thorpe sends none.

## 5. Static level entities, the bag and the chest

**Observed** (ftue-static-entities session note; table `local-data/content/ftue-entities-25810808.json`
made by `content-tool ftue-entities`). The retail server sends 20 static level entities of the
tutorial a spawn card that names only a `GdeRef` (`01 02 <GdeRef>`), with recorded states of some
components. A static `GdeRef` is 8 fixed level bytes and then the session's tutorial phase id,
byte-reversed. `FtueIslandComponentReplicatedState` (3194) bodies hold the player's `GdeRef` and
facet entity. The server replays these states with the session's ids
(`bins/sim/src/world/statics.rs`) for the entities carded after the move into the water. The
ship's are not sent: with them the live client showed a black world on the ship (2026-09-12).

**Observed.** The bag (`2fb40065…`): the client sends 2939 with the bag's id; retail sends the
container change, completes the bag's delayed events after 134 ms and 1282 ms, and stops its slot
after 1298 ms. The server does the same.

**Observed.** The chest (`efa5a09f…`): the client starts the use in mask bit 8 of an input record
(1589): the chest's `GdeRef`, facet entity, an interaction id and the kind (14). The server answers
3743 (accepted, `@LootContainer_AncientSmall_DisplayName`), sets the chest's interact state, and
after 2.023 s sends 3746, the interact state back, 2724 and two `PotionHealthT1`. **Unknown:** the
last 16 bytes of 3743; **Our choice:** an id derived from the player and the interaction.

## 6. Mob states: number, clock and animation

**Observed** (Ghidra, read only). The client copies each mob state into an action state
(`FUN_142a2bc70`): state number = `idAbs` (25) and `idRel` (0); state time (µs) = `timeAnchor`
(child 40) + (`timeOffsetAbs` (26) and `timeOffsetRel` (1), ms) × 1000. The remote update
(`FUN_144872150` → `FUN_144871ad0`) takes a render time from a µs clock (`FUN_142a28270` returns
a value / 1000) minus a delay, and blends between the two buffered states around it
(`FUN_142a23280`). With no state at or before the render time, it shows the oldest state. When
the newest state is old, it calls `FUN_144873260`, which resets the animation scopes (and all of
them after 10 s).

**Observed** (live capture). `TimeSynchMsg` is ns since 2000-01-01; a mob's `timeAnchor` is µs
since 2000-01-01. Retail sailor 1: first state number 2, anchor at its spawn; then one state per
333 ms while it waits (number +1, offset +333 ms, child 28 +10), one per 33 ms while it walks;
the anchor moves on after 60021 ms. Child 40 comes only with the first state and a new anchor.

**Observed** (live, 2026-09-12). With the server clock in ms, the client render time was before
every mob state: the sailor stood at its card position. With the clock in ns and states numbered
from the server clock (every tick, anchor every state), no tutorial mob was visible.

Implemented (`bins/sim/src/world/mob_timeline.rs`, `AlcState::mob_first` / `mob_step`): each mob
has its own timeline as retail (number from 2, own anchor, 333 ms / tick / 67 ms periods, new
anchor after 60 s). For the tutorial sailor (`Corrupted_Sailor_1H_Club_FTUE`), the states also
carry the full-body fragment (spawn, idle, combat idle, walk, attack, death), its time in 1/30 s
ticks and the tags, with the retail entry bytes (mob-animation session note, section 4).

**Our choice:** the attack fragment starts at the server hit and plays 1.5 s; a mob that moved in
the last 333 ms plays the walk.

**Observed** (rotation-encoding session note). Child 11 is the yaw as a quaternion: tag `0x66`
(w derived, one byte from z), tag `0x46` with the sign bit (z derived, one byte from w) or `0x6e`
(north). The byte is round((v + 1/√2) / (√2/255)); yaw = atan2(−dx, dy).

Implemented: facing (child 11, turning 9° per state); the fragments, actions and reaction
fragments of the sailor, legionnaire, swarmer, risen and Thorpe (creature-animation session
note); a hit reaction plays on a mob hit, with a limit of one in 2 s; a mob card carries the
spawn fragment at first sight; a mob timeline starts only when its owner has spawned.

**Observed** (decoded retail states: sailor 35050, legionnaire 204711). The first state in which
a mob sees its target plays its alert (`Alert_Turn` or `Alert`, 883 to 1066 ms) and starts the
look pose: child 32 = the FullBody bit plus the look pose bit (sailor and legionnaire 4, swarmer
3, risen 2), child 33 = the alert entry and the look pose entry with the same start number,
child 29 = the look pose time as a big-endian `half` (`02 3556`, 0.333 s), child 35 = the angle
to turn (`aiAngleToDesiredFacing`, legionnaire 1.774 rad). The look pose stays on while the mob
fights; its time goes in child 29 of every later state. Thorpe has neither.

Implemented: the alert (1 s; the mob stands while it plays, and runs after it), the look pose
with its time in every state, child 35 with the alert and 0 at the next fragment.

**Not done:** the additive reactions; child 42 (`0x33` in both retail alerts, meaning
**Unknown**); retail drops the look pose during the sailor's and the risen's attacks.

**Later finding.** `crates/nw-messages/src/alc.rs` names child 42: the high byte of the client's
state flag word (`+0x450`), whose low byte is `dataBits` (child 41). The meaning of each bit of
`0x33` is still **Unknown**.

## 7. Sailor damage, pace and block

**Observed** (live capture). In the whole ship fight only two sailor hits did damage (3601 at
80504 and 101406), 12.97 each; the player's health went 856 → 852.61. Each 3601 says the
player's armor kept 0.6006 of the damage. The sailor's rows: level 1 base damage 145.52, damage
mod 0.5 (`VitalsBaseData`), category `Solo_StarterBeach` damage mod 1.0. So the attack factor is
12.97 / (145.52 × 0.5 × 0.6006) = 0.2968.

**Observed.** Sailors swing about every 6 s (sailor 2 at 87749 and 93546) and the first swing
comes about 6 s after the fight starts (sailor 1: released 73715, swing 80048; sailor 2: card
81629, swing 87749). Our generic brain swung every 2 s from the first tick, at 73 damage: the
player died.

**Observed.** While the player holds block, every input record has bit 62 of slot value 1 set;
the server then sends the player's `DamageReceiver` `isBlockActive` 1. All 10 melee hits that
retail reported as blocked (2218 `HandleMeleeDamage` with byte 1, no 3601) fall in such periods.
A hit that does damage sends 3601 and 2218 with byte 0. (One damaging hit, 486029, came while
the bit was set; **Unknown** why, perhaps a hit from behind.)

Implemented (2026-11-05): the player armor is rated from its worn pieces (`ArmorItemDefinitions`,
gear score ^ 1.2) against the mob's level gear score; the four starter pieces keep 0.6091, 1.4 %
from the retail 0.6006 (`SimWorld::player_armor`); the ship sailors
hit with factor 0.2968, every 6 s, the first 6 s after their fight starts (`SAILOR_PACE`); a mob
hit on a blocking player does no damage and sends 2218 with 1, else 3601 and 2218 with 0.

**Observed** (xp-potion-exit session note). A dodge sets bit 3 of slot value 1. Retail sends no
damage for a hit that comes in the 500 ms after a dodge. Each creature has its own pace: attack
factor, gap between swings and the time from swing to hit.

Implemented: dodge (500 ms, no damage); per-creature pace (`SAILOR_PACE`, `LEGION_PACE`,
`RISEN_PACE`, `SWARMER_PACE`, `THORPE_PACE`); a swing does its damage after the hit delay of its
creature; Thorpe heals in an intermission of 6.634 s at 60 % health and the fight ends at 50 %.

## 8. Quest tracker, XP, potion and the exit

**Observed** (objectives-state and xp-potion-exit session notes).

- `ObjectivesComponentReplicatedState` (type 3857, own component 22), slot 1: child 1 the
  tracked objective, child 2 the completed objectives, child 3 the active objectives (56-byte
  records), child 4 the tasks (18 bytes each, index in post-order of the task tree). A list is
  `00 01 <version> <count>` and the entries.
- `ProgressionComponentReplicatedState` (type 899, own component 70): `02 03 xp rested` at
  login, `02 01 xp` after a kill; 20 XP per kill.
- `CooldownTimersComponentReplicatedState` (type 2932, own component 45): the consumable
  cooldowns (consumable, end µs, group, consumable again).
- A potion: quick slot 1 sets bit 23 of slot value 1. Retail sends `AdjustFXScripts` (608) on
  the status effects component, heals the player and uses one item of the stack.
- At the end of the tutorial retail sets 17 lore achievements, fills the potions and starts the
  open-world quest `12_GetOffBeach`; the player arrives in the water at (798, 337).

Implemented: the quest tracker sends the objectives state at spawn and after each task change,
and the three retail states at the end; XP per kill; potion drink (326 ms) with heal, cooldown
and stack count; the 17 lore achievements, the potion refill, the water teleport and the start
of `12_GetOffBeach` (trigger radius 18 m).

## 9. Stamina

**Observed** (stamina-wire session note). Only three things take stamina: a dodge on land (45), a
dash in the water (50) and a mob hit on a blocking player. Each starts the 1 s regen delay: child
3 counts the seconds down to 0, then `cur` grows by 33.33 per second. A land dodge starts the
delay once more 500 ms after the press; so does each release of the block key. A blocked hit
takes 73 x the attack's `BlockStaminaDmgMod` for the signifier and 91.25 x for Thorpe, with the
capture's warhammer (block stability 27); the cause of the two factors is **Unknown**.

Implemented: the stamina state (component 42) with these rules; a dodge with too little stamina
does nothing; a player swims within 1.6 m above the level's `oceanLevel` (Inferred: retail swam
at 20.0 to 21.5 m over a sea at 20 m, and walked onto the beach at 23.5 m); a block holds while
stamina is above 0.

## 10. Boss phase, storage search, quest handoff and moves

- **Observed.** Thorpe's `BossPhaseComponentReplicatedState` (type 403, component 14) comes as
  `01 03 00000000 01` with the card, `01 01 00000001` at his intermission and `01 01 00000003`
  at his end. Implemented.
- **Observed.** 7048 `GlobalStorageComponentClientFacet_ReceiveStorageSearchItems` is the
  header and 22 zero bytes, sent before the bundle of each item change. **Observed** (live
  runs 2026-09-14): our header (the facet entity + `0x2296cdd350b5c51c`, read off the retail
  bytes as for 608) made the client log `RMIDispatch: The facet target ID couldn't be found` at
  every send, so the component is not at that id; where it is is **Unknown**. Not sent.
- **Observed.** Retail moved the player into the water with three own states in a row (118546,
  118580, 118614): the place with `worldPosRel` `ffffff` and facing `66f6`, the place again at
  zero offset `7f7f7f` with teleport number 2, then zero offset and the number alone. A single
  state with the number left the live client moving on with its ship speed: thrown 18 m east
  and 3 m up (2026-09-14). Implemented: the three states, one per bundle.
- **Observed.** In every live run the first spawned mob (number `MOB_ENTITY_BASE`, low 32 bits
  0) stayed invisible while it fought; the next mobs, with the same card bytes, showed. Mob
  numbers now start one above the base. **Inferred:** the client drops a `GdeRef` whose low
  32 bits are 0.
- Area tasks use a circle around `OverrideLocation`, sized per `TP_TargetTag` just above the
  distance at which retail fired it (EnterRuins 7 m, First_Fight 5.5 m, ChestDiscovery 4.5 m,
  ForClimb 25 m, TF 20.5 m; others 18 m). One 18 m circle fired the ruins trigger through the
  closed door (2026-09-14). **Not done:** the real trigger volumes (client level data).
- Implemented: a handoff carries the open-world quest's task states, timers and the tracker
  version; the next sim goes on with them.
- The sim read the oldest position of each input message; the client sends the newest record
  first. It now reads the newest.
- **Observed** (live run 2026-09-14). The move limit of 7.15 m/s refused moves in fights with many
  dodges: the retail capture moved 17.1 m in 1 s, 37.2 m in 3 s and 61.8 m in 6 s. The limit is
  now 10.4 m/s (`speed_margin` 2.2). A refused move keeps the server's position, so mobs then
  chased a stale player position.
- **Observed.** Every tutorial creature's first hit sends its vitals with `healthChangeFlags` 3
  (`01 09 <health> 03`); retail cards send 0. Implemented.
- **Observed.** Retail sailor 1 came 15.9 s after the player's first own state (`Wave_01`).
  Sent at once, its only card came before the client could show it. It now comes after that
  delay.

## 11. Mob looks, lock-on states, gait, combat status and the exit (2026-09-16)

Times are ms from the first record of the retail tutorial capture.

- **Observed.** The two legionnaires share `Legion_Legionnaire_FTUE` but not a look: card asset
  `c05fc957…` at 179393, `87a335e7…` (signifier) at 202592. The prefab table and the looks now
  keep each prefab by vitals id and asset; each tutorial mob gets the asset of the retail mob at
  the same spawn order place.
- **Observed.** A live creature's first vitals carry `vitalsData` `00 <server ns> 0 ff`, the
  server clock at the card (sailor 1: `0bbca3350847b1bf` = 13:04:38.206 at 16198). Sent now.
- **Observed.** Right after each creature first picks the player (2415), retail sends its
  component 4 `MB::LookTargetingComponentReplicatedState` (2443, `currentLookAtTarget`):
  `01 01 bfa96f3ea2e36a5a`, the player's facet entity, for all 10 tutorial creatures; never
  cleared at a 3916. Read, written and sent with the card and after each new brain target.
- **Observed** (9 fights). Own component 28 `CombatStatus` (2913) comes about 40 ms after each
  player hit on a mob: logged-out time = hit + 20 s, concluded time = hit + 10 s (server ns), the
  in-combat flag 1 with the first hit. When every hit of the step killed, concluded = hit + 1 s;
  at the concluded time the flag 0 comes alone. A mob's hit on the player (3601) changes nothing.
  Implemented. **Unknown:** what set the flag at Thorpe's start (455733, no player hit).
- **Observed** (full decompile, `local-data/re-sweep`). The client lock test of
  `Javelin::CameraLockTargetComponent` (`FUN_142c95b40`; the same checks inline in
  `FUN_142c932a0`) refuses a target when one of these is true:
  1. the target's flag `m_isNotTargetableWhenDead` (+0x168) is set and its vitals handler
     (+0x260) answers true at vtable slot +0xc0;
  2. `m_isNotTargetableWhenDeathsDoor` (+0x169) is set and the same handler answers true at
     slot +0xc8;
  3. an entity-addressed bus call on the target with the value 26 answers true
     (`FUN_142c95760`; other callers pass 11 and 29; the bus and the meaning of 26 are
     **Unknown**).
  The lock and switch code also needs the target entity state 4 (`FUN_141720bf0`). The +0x260
  handler is the first handler on the target entity's vitals bus (`FUN_142c98010`, slot set at
  activation from `m_vitalsCompReqId`). The sailor slice sets both flags to 1 (ship-mobs note).
- **Inferred.** The only server inputs to the test are the creature's vitals state (dead and
  deaths door) and the unnamed state 26. The vitals path now matches retail (`vitalsData`,
  health above 0). Slots +0xc0/+0xc8 are dead / deaths door by the flag names; the exact field
  each slot reads is **Unknown** (the vitals handler vtable has no RTTI symbol).
- **Unknown.** Whether the three states above fix lock-on; only a live test can tell.
  **Later finding.** They did not. The cause was the mob ALC `dataBits` (child 41 bit 0 = targetable; child 42 = high byte of the client flag word at `+0x450`). Commit `110ccc0` sends retail's `d3 13` (`awake_data_bits` in `crates/nw-messages/src/alc.rs`).
- **Observed** (ship-mobs note). Retail sailors started swings at 0.89-1.81 m, ran only to close
  gaps of 4-12 m, walked or stood nearer, wandered at 0.6 m/s, and stood about 2.0 s after the
  alert. Each tutorial kind now has a gait (swing start, run distance, walk and wander speed)
  and its slice sight (sailor 30 m, legion, risen and swarmer 12 m, Thorpe 20 m). The open world
  keeps one reach and one speed.
- **Observed.** The island foes send 2415 when they first see the player (legionnaire 25 s after
  its card, player 3.7-8.1 m away; signifier 56 s, 9.8 m; risen group 7-9 s). Implemented; the
  sailors and Thorpe keep theirs at spawn (retail 0.3-0.6 s after the card).
- **Observed.** Spawn delays after the cause: legionnaire 4200 ms after the door, signifier
  5782 ms after its trigger, risen +417 ms and swarmers +1315, +2316, +3313 ms after ForClimb,
  the last ship sailor 1 s after the first (slice wave delay). Implemented.
- **Observed.** After the 502, retail sends 3951, then the quest-end, lore and potion bundles,
  then the empty 6310 361 ms after the 502. The open world's first full achievement state holds
  the tutorial's 54 bits. Implemented: the handoff waits 361 ms and carries the set bits.

- **Observed.** `MB::GatheringComponentReplicatedState` (81, own component 36) has slot 1 with
  nine children: status u8, amtRemaining f32, gatherEfficiency f32, numGathering u8,
  gatherRefCount u32, waterGatherRefCount (width **Unknown**: in no body), isGathering flag,
  gatherableGDEID u64, syncGatherRefCount u32. The split fits 114 different bodies of the
  tutorial and two open-world gathering captures, each ending where the next member or group
  starts. A grouped mask byte comes after the data of the children of the one before it.
- **Observed.** The chest gather: at the accept (237308-237342) own 26:3752 names the chest
  facet and 36:81 runs the gather (status 1, 2.5 left, efficiency 1.0, no other gatherer,
  counters 1, gatherable `00000000ffffffff`); at the end (239343) 36:81 says done (status 3, 0,
  0, counters 2, the chest facet). Implemented with readers for 81 and 3752
  (`MB::InteractorComponentReplicatedState`: slot 0 child 0 the target, slot 1 child 0
  `m_enabled`; 1358 bodies). The counters of later gathers go on by one per state (Inferred).
  Not sent: 54:100 (`replicatedHideLevel` of the name tag, 1 during the gather; not part of the
  gather, and our player card has no component 54).

## 12. Live test of 2026-09-16 and its fixes

Live run 19:16-19:25Z (tutorial to the boss cave). Retail source: the tutorial ledger.

- **Observed.** The FTUE heightmaps are 1024 x 1024 (one sample per metre, `regionSize` 1024); the
  map tool read only 2048 x 2048, so the tutorial world file had no terrain. Their raw / 64 puts
  the retail creatures' cards 0.5 to 1.1 m above the ground (Inferred: `mountainHeight` 1024).
  The tool now reads both sizes and stores every grid at 32 units per metre.
- **Observed.** Retail creatures do not follow the height map. About 50 to 100 ms after the card
  each drops to its fight's floor and stays on it: ship deck 89.42, ruin top 26.45, courtyard
  20.33, risen and swarmers 21.36, Thorpe 22.21 (his arena has no terrain). Our mobs kept their
  card z (0.4 to 1.0 m higher; floating). Tutorial mobs now spawn on these floors and keep them.
- **Observed.** Our 3601 sent zeros. Retail: attacker facet entity, flags `02 00`, damage / max
  health as `f32`, the attacker's x, y, z, item tag (sailor 6, legion 5, risen 6, swarmer 4,
  Thorpe 4) and scale (sailor 0.6006, others 0.5612). Sent now. **Inferred:** our zeros made the
  client stagger the player at every hit (hammer swings every 2.5 to 3 s; retail hits do not
  break a swing).
- **Observed.** Each 1589 repeats older records with fewer mask bits (counters 7290, 7289, 7288);
  read newest to oldest they made a false attack edge in nearly every message. Each record now
  counts once, oldest first, by its counter.
- **Observed.** Retail dropped a creature's swing when a player hit made it reel before the swing
  landed (205594, 207659, 296654, 305722); retail hits landed 0.77 to 2.19 m from the body. A
  reeled mob's swing now drops, and a swing misses beyond its gait's swing start + 1 m.
- **Observed.** `PotionHealthT1` has no `ItemStatsRef`, so the heal lookup by stats never found
  it. Consumables are now found by their own id (400 + 10 %, cooldown 25 s).
- **Observed.** Retail has no health regen in the tutorial (health only falls, except the potion);
  open-world regen starts about 3 s after the exit (rule **Unknown**).
- **Observed.** The boss cutscene (6703 `c723c42d`) is started by the client when component 10 of
  `ftue_v2_enc_thorpe` turns active. Our area trigger (asset 15) was hidden 2.1 s before its
  `010108` change, so the change was lost. A hide now follows the last change before it.
  **Unknown:** whether the lost change was the cause.
- **Observed.** The client lock test (`FUN_142c866e0`) passes every gate that reads mob data:
  vitals bits from `vitalsData` head (1 dead, 2 deaths door), game-mode flag 26
  `CannotBeTargeted`, hostility (an AI target is an enemy). **Unknown:** which player-side,
  line-of-sight or score gate fails; one instrumented run must show it.
  **Later finding.** This was wrong. The test also reads bit 0 of the mob's state flag word, and
  our mobs kept the fresh card's `10 10`. The cause was the mob ALC `dataBits` (child 41 bit 0 = targetable; child 42 = high byte of the client flag word at `+0x450`). Commit `110ccc0` sends retail's `d3 13` (`awake_data_bits` in `crates/nw-messages/src/alc.rs`).

## 13. Combat and boss work of 2026-09-16 (after the second live video)

Sources: the tutorial ledger, the game paks (`javelindata_damagetable*`,
`javelindata_itemdefinitions_weapons`, the animation databases, `commander_controllerdefs.xml`,
the creature slices) and Server.pak behavior trees. Not live-tested yet.

- **Observed.** The blunderbuss fires through 2719 `RequestSpreadshot`; the server answers with
  2570 (the request echo, the CRC-32 of `blunderbuss_primary_attack`, config 230 for six pellets)
  and one pellet 2071 per hit. We ignored 2719, so the blunderbuss did no damage. Implemented.
- **Observed.** Each weapon attack has a damage clip (`CAGE-Damage`) live for about 0.1 s and a
  homing clip (radius 3.75 or 4.5 m, 45 or 60 degrees). Implemented as a wedge per weapon kind
  (reach 1.6 to 2.6 m; the reaches are Inferred or our choice), hit in the middle of the damage
  window after the press (light) or the release (heavy), up to 5 mobs (our choice). Scored on the
  retail swings, a 1.6 m, ±45 degree wedge kept 67 of 67 retail hits.
- **Observed.** Each weapon kind has its own damage rows (`Warhammer_PrimaryAttack`,
  `Warhammer_HeavyAttack` 1.3, ...). Implemented.
- **Observed.** The 1589 target row (row 3) holds the locked target. Retail locked on only from
  the first legionnaire (202 s), never on the ship. Logged (`lock target`) and used to aim the
  swing. **Unknown:** why our client did not lock on.
  **Later finding.** Solved by commit `110ccc0` (mob `dataBits`, §15.3).
- **Observed.** Creatures lunge in their swing until the bodies touch; all 17 retail mob hits
  landed 0.79 to 1.65 m from the player. Implemented per creature (capsules: sailor 0.4, island
  foes 0.5, Thorpe 0.7, player 0.4 m).
- **Observed.** Thorpe's attacks: Stab 01/02, Swipe L/R (phase 1); Stab 02, Swipe R, Flurry (three
  hits), Overhead (phase 2); cooldown 1.0 to 1.35 s after each; phase 2 does 2.2 x damage with a
  fire part (3601 second item, tag 9; byte-checked with 513963). Fight tags `08 01 0b` until his
  first reaction, `08 81 13` in phase 2. Implemented.
- **Observed.** Force grab (every 11 to 12 s at a player 6.6 to 13 m away): fragment (47, 14,
  0xb8), the table's spell slices `thorpe_force_grab_attach` and `thorpe_force_grab` with his id
  and the player's place, then a pull to about 2 m in front of him 1735 ms into the grab.
  Implemented; the pull is a plain position path. **Unknown:** whether the client needs the
  slayer-state child (13 = 4) during the pull.
- **Observed.** Transition: `Phase_Transition_1_to_2` for 10.4 s; a shockwave at +7748 ms (2218,
  no 3601, the player 5 m back); then `Combat_Tell` and the shoulder charge (25.7 m/s, outro hit
  0.38, half fire). Implemented; the push length (282 ms) is our choice.

## 14. Not done

- The spawner static's changes.
- The 250 to 280 ms wait before regen after a water dash (cause **Unknown**).
- The general formula of the stamina a blocked hit takes.
- The additive reactions, child 42 of the alert state.
  **Later finding.** `crates/nw-messages/src/alc.rs` names child 42 (the high flag byte, §6).
- The body part of a player hit (2071 bytes 40-43, CRC-32 of the bone name): we always send the
  torso, which retail sent on 25 of 70 hits.

## 15. Lock-on in the retail tutorial ledger (2026-09-18)

Source: the newchar ledger of 2026-09-12 (`local-data/captures/own/newchar-linux-2026-09-12`).

- **Observed.** Five lock runs (input 1589 row 3), the first on the first legionnaire from
  201.9 s to 209.5 s (raw id `26d1dfc900f49503330627c5c3df6701`). The row's `u64` is the first
  8 raw bytes read little-endian, the client's entity id.
- **Observed.** The middle mouse button sets input value 2 bit 20, value 5 bit 29 and value 7
  bit 51 in one record, and that record already names the target.
- **Observed.** The legionnaire's retail card (12 members) matches ours member by member,
  except its GDE ref (a GUID; ours is built from the entity number, and our bodies use the
  byte-swapped number wherever the client's entity id goes) and its max health.
- **Observed.** Retail max health drops the fraction: sailor 353, legionnaire 370 (ours was 354
  and 371). Fixed.
- **Observed.** Before the first lock the server sent no targeting, aggro, faction or game-mode
  message. Own changes were stamina, the interactor slot-1 flag, vitals, objectives, paperdoll
  and achievements. The retail own ALC carries `slayerSeqTimeRel` (child 2) in almost every
  state and the slayer children 13-24 when they change; ours sends neither.
  **Later finding.** `crates/nw-messages/src/alc.rs` now sends child 2 (`SEQUENCE_TIME_REL_CHILD`)
  and the slayer children 13 to 24.
- **Unknown.** Which client gate rejects our targets. The lock test is a virtual of each
  `CameraLockTargetComponent`; its gates read client data that the ledger cannot show. A
  runtime probe must show it.
  **Later finding.** Solved: the mob ALC `dataBits` (child 41 bit 0 = targetable; child 42 = high byte of the client flag word at `+0x450`). Commit `110ccc0` sends retail's `d3 13` (`awake_data_bits` in `crates/nw-messages/src/alc.rs`).

### 15.1 The lock gates, named (2026-09-18)

- **Observed.** The gate `FUN_142c95760` sends query 26 on the entity of the candidate target
  (the mob) through `FUN_142c77f30`, an event of `GameModeParticipantComponentRequestsBus`. The
  enum (reflection `FUN_1432d1d20`) names 26 `CannotBeTargeted`; the bus's other queries match
  the same enum: 0xb `IgnoreGatheringTradeskillRequirements` (the gather check), 0xd
  `IsInBossFight`, 0x17 `SendDeathRecapToAllClientsInAOI`, 0x1f `DisablePlayerInteracts`,
  0x20 `PlayerCutsceneIgnoredByAI`.
- **Observed.** Neither the retail legionnaire card nor ours carries a game-mode participant
  member, so the query has no state to answer true from. **Inferred:** not the gate.
- **Observed.** The lock component's fields (reflection `FUN_142c9dd00`): `m_isNotTargetableWhenDead`
  0x168 and `m_isNotTargetableWhenDeathsDoor` 0x169 (the gate's +0x110/+0x111 from +0x58), the
  vitals, player, marker, groups, ownership and stealth-invisibility request ids (0x180-0x1a8),
  and `m_notTargetableOnInit`.
- **Observed.** Targetable is switched by the CAGE activity `SetCameraLockTargetTargetable`
  (`ConditionalActivityData<SetCameraLockTargetTargetableData>`). In all 748 action lists of the
  game paks it sets "not targetable" only in death actions (`r_Death*`), `Teleport` and
  `Phase_2_Intro`; no spawn, idle or move action does. **Inferred:** our first mob state, which
  starts the spawn fragment where retail's legionnaire started none, does not make it
  untargetable.
- **Unknown.** The gate that rejects our targets is still not found from static data. Remaining
  candidates are client state the ledger cannot show (the relation registry of `FUN_14171b010`,
  `m_notTargetableOnInit` per prefab, the stealth/ownership requests).
  **Later finding.** None of these was the gate. It was the mob `dataBits` (commit `110ccc0`, §15.3).

### 15.2 Our ledger against retail (2026-09-18, live run 8)

- **Observed (Azoth capture of our server, same tutorial, greatsword + blunderbuss).** The MMB
  press sets value 2 bit 20, value 5 bit 29 and value 7 bit 51 in both ledgers. Retail's row 3
  names the target at the press and keeps it 7.6 to 13.9 s. Ours: 60 presses, 2 targets (0.35 s
  and 1 s), and the first lock ended with no press and no server event for the mob.
- **Observed.** Server message types that only retail sent before the first fight are social,
  guild, voice chat, inventory, anti-cheat and meta achievement messages; none is about targeting.
- **Observed (own player ALC, 195-215 s retail vs 225-240 s ours).** Retail sent the own
  `rotation` (child 11) in 938 and `lookDir` (child 12) in 408 of 1,193 own states, and child 2 in
  1,179. Ours sent 11 and 12 in none of 450.
- **Inferred.** Without the own facing and look direction the client's lock holds only while the
  player still looks along the stale replicated look. Own states now carry both from the newest
  input's view when it changed. Unknown: the client line that reads them; the effect of child 2,
  the own state rate (retail about 60/s, ours 30/s) and our `idAbs`/`timeOffsetAbs` in every state.

### 15.3 The lock gates in order, and live run fishing-01 (2026-09-26)

- **Observed (fishing-01, our server, 2026-09-26 15:50).** 74 lock-key presses, no lock. The
  same ledger file starts with a retail session (2026-09-24 17:40) that locked 15 times; count by
  time, not by file. Our older lock-test ledgers (2026-09-18/23) locked only the signifier
  (`87a335e7…`), 2 to 7 s at up to 20 m; sailors, the legionnaire and the swarmers never locked
  at 3 to 21 m. Retail locks all of them.
- **Observed.** A tutorial sailor's spawn group carries the same 13 member classes in retail
  and ours (10, 11, 12, 15, 1528, 3139 × 7, 3183); the card differs only by its random GdeRef,
  the vitals time and the position.
- **Observed (client decompile).** Key `FUN_142c95e20` (camera-lock enable flags from the weapon
  combat profile) → press `FUN_142c93e60` (disable counter 0, UI state "hud",
  `FUN_142541190`) → scan `FUN_142c8bbd0` → per target `FUN_142c866e0`: transform, targetable
  byte (`+0x249`), dead / death's door, `CannotBeTargeted` (query 26), relation flags
  (`FUN_14171b010`), hostility (`FUN_142a9b4a0`), range 50 m (`m_maxLockRange`), line of sight
  `FUN_142c95000` (required: the player slice sets `m_requireAllTargetsVisible`). The ray tests
  the "static" and "Terrain" layers and passes when it hits nothing or the target itself.
- **Inferred.** The signifier's slice names no character model; the others do. The best lead is
  the line of sight: the ray hits the mob's own body and the hit does not resolve to the target.
  The relation flags are the second lead. **Unknown**: which gate fails; a breakpoint on the
  return paths of `FUN_142c866e0` while pressing on a sailor will show it.
- **Later finding.** Neither lead was the cause. A live gdb run showed that the client copies the
  mob state over its own, and the lock test reads bit 0 of the flag word. The cause was the mob ALC `dataBits` (child 41 bit 0 = targetable; child 42 = high byte of the client flag word at `+0x450`). Commit `110ccc0` sends retail's `d3 13` (`awake_data_bits` in `crates/nw-messages/src/alc.rs`).
