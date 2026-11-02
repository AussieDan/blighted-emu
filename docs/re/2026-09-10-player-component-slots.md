# Player component state slots and the empty-state crashes

Date: 2026-09-10.

Builds: Steam build `22469132` (old, Ghidra project `<re-scratch>/ghidra`) and Steam
build `25810808` (current, project `<re-scratch>/ghidra-25810808`), image base
`0x140000000`. Constructor addresses below are old-build unless marked `25810808`; the slot
counts and wire grammar are the same on both builds.

Offline only. The game was not run. Easy Anti-Cheat was not analyzed. No body bytes, names,
ids, positions, tokens, or IP addresses are in this note. Capture data was used only as
counts, sizes, decoder addresses, and slot-bit indexes.

Labels: **Observed**, **Inferred**, **Unknown**.

Method. All constructors were found on the old build by listing every caller of the
child-register function `FUN_141775c60` and of the add-slot function `FUN_141677dd0`
(Ghidra headless, read-only, analysis off, several parallel jobs on per-process project
copies). Each constructor was matched to its class by the field-name strings it registers
(per-class field lists in `local-data/azoth-catalog/packets/<type>.json`), then checked
against the class handler addresses in `<re-scratch>/scratch-pc/classes-old2.tsv`
(the constructor always sits in the same code cluster as the class `CreateInstance` and
`Unmarshal` entries, and the `Unmarshal` thunks call these constructors directly). Add-slot
call counts were counted twice: in the decompiled text and as raw `CALL` instructions; the
two counts agree.

## Answer

**Observed.** No player component class has more than 8 slots. Every class in the table has
1 to 4 slots. The generic reader therefore reads exactly one slot byte for every one of the
56 classes, and the correct "nothing set" body is the single byte `0x00` for each of them.

**Inferred.** The crash is not a wrong empty body. `0x00` is wire-correct. The client crashes
because a component that never gets a state record keeps a raw, unconstructed allocation as
its state object, and the apply and merge paths dereference it as a constructed state. The
retail server never sends an empty body for these classes; in the capture every one of the
56 component records carries a multi-byte first body (smallest seen: 3 bytes).

## 1. Slot model and the generic reader

**Observed.** A replicated-state object has a vector of slots at `+0x10`/`+0x18` (begin,
end). One slot is 800 bytes (`0x320`). The base constructor `FUN_14162f3e0`
(current build `FUN_14162fff0`) calls the add-slot function once, so every state has slot 0.

**Observed.** The add-slot function `FUN_141677dd0` (current `FUN_1416789e0`) returns the
slot count, appends one zeroed 800-byte slot, and stops at a cap of 10: `if (9 < count)
return 10`. Constructors keep the returned index in a member and pass it as the slot index
to `FUN_141775c60` when they register each named child.

**Observed.** The generic bundle reader is virtual `+0x90`. On the old build it is
`FUN_1417b4110`; on the current build `FUN_1417b5080`. It computes
`slotCount = (obj+0x18 - obj+0x10) / 800`, then reads one byte per group of up to 8 slots:
byte 0 covers slots 0-7, byte 1 covers slots 8-9. The read is positional, not driven by a
continuation bit: `ceil(slotCount / 8)` bytes are consumed always. A set bit reads that
slot's child list through `FUN_1417b5330` (current) / child virtual `+0x30`; a clear bit
reads nothing.

**Observed.** So an empty body is `ceil(slotCount / 8)` zero bytes: `0x00` for 1-8 slots,
`0x00 0x00` for 9-10. No player component reaches 9 slots.

**Observed.** The capture ground truth agrees: in
`local-data/captures/open-world/NH-MSQ-10.2-Quest-61c0a783ba281885/fragments.jsonl` every
type index of the 56 classes (and also type 10, the spawn card) was decoded with RVA
`0x17b4110` (the generic reader) on every fragment. Only type 11 used its custom decoder
`0x2a436b0`.

**Observed.** For each of the 56 classes the qword at `vtable + 0x90` of the vtable the
constructor stores at `*object` is `0x1417b4110`. Verified on all 56 state vtables on the
old build.

## 2. The 56 classes

`adds` = `FUN_141677dd0` calls in the constructor after the base constructor. `slots` =
1 + adds. `bytes` = slot bytes the generic reader consumes. `empty` = the "nothing set"
body. All entries: reader `+0x90` = `FUN_1417b4110` (old) = `FUN_1417b5080` (current),
capture decoder `0x17b4110`.

| comp | type | class | ctor (old) | adds | slots | bytes | empty |
|---:|---:|---|---|---|---:|---:|---|
| 0 | 10 | GDE spawn card (not a component state class) | none | none | none | none | real body only |
| 1 | 6951 | Javelin::PointsAccumulatorComponentReplicatedState | `0x143bb7090` | 0 | 1 | 1 | `00` |
| 2 | 5437 | MB::ItemManagementComponentReplicatedState | `0x142d345a0` | 1 | 2 | 1 | `00` |
| 4 | 2406 | Javelin::PlayerArenaReplicatedState | `0x14276ef30` | 0 | 1 | 1 | `00` |
| 5 | 3290 | MB::PlayerTimeComponentReplicatedState | `0x14407de10` | 0 | 1 | 1 | `00` |
| 6 | 2768 | MB::PlayerHousingReplicatedState | `0x143569be0` | 0 | 1 | 1 | `00` |
| 9 | 3935 | MB::PlayerComponentReplicatedState | `0x146711e90` | 2 | 3 | 1 | `00` |
| 10 | 3829 | MB::ObjectiveInteractorComponentReplicatedState | `0x143a1b9e0` | 0 | 1 | 1 | `00` |
| 12 | 1195 | MB::PlayerAppearanceComponentReplicatedState | `0x146710240` | 0 | 1 | 1 | `00` |
| 13 | 603 | MB::AudioProxyComponentReplicatedState | `0x1427b6600` | 0 | 1 | 1 | `00` |
| 14 | 1528 | MB::DamageReceiverComponentReplicatedState | `0x1466f8310` | 0 | 1 | 1 | `00` |
| 16 | 11 | MB::ALCReplicatedState | custom decoder | 2 slots | 2 | 1* | `00`* |
| 17 | 3563 | MB::TemporaryAffiliationReplicatedState | `0x14405c370` | 0 | 1 | 1 | `00` |
| 18 | 5338 | MB::FishingComponentReplicatedState | `0x1430da340` | 0 | 1 | 1 | `00` |
| 19 | 3133 | MB::EntitlementComponentReplicatedState | `0x14302c5d0` | 0 | 1 | 1 | `00` |
| 20 | 3652 | MB::PlayerHomeComponentReplicatedState | `0x1434e2e40` | 0 | 1 | 1 | `00` |
| 22 | 3857 | Javelin::ObjectivesComponentReplicatedState | `0x143a1df90` | 1 | 2 | 1 | `00` |
| 24 | 3312 | Javelin::GameModeParticipantReplicatedState | `0x14329acb0` | 2 | 3 | 1 | `00` |
| 25 | 1292 | MB::CampingComponentReplicatedState | `0x1440c6ad0` | 0 | 1 | 1 | `00` |
| 26 | 3752 | MB::InteractorComponentReplicatedState | `0x1449193b0` | 1 | 2 | 1 | `00` |
| 27 | 129 | MB::AttributeComponentReplicatedState | `0x14276c250` | 0 | 1 | 1 | `00` |
| 28 | 2913 | MB::CombatStatusComponentReplicatedState | `0x1466f1f10` | 0 | 1 | 1 | `00` |
| 29 | 6234 | Javelin::InstancedSlayerScriptReplicatedState | `0x1432e8900` | 1 | 2 | 1 | `00` |
| 31 | 1927 | MB::ReactionTrackingReplicatedState | `0x146717260` | 0 | 1 | 1 | `00` |
| 32 | 2895 | MB::MagicComponentReplicatedState | `0x146706d10` | 0 | 1 | 1 | `00` |
| 33 | 982 | MB::LootTrackerComponentReplicatedState | `0x1437350d0` | 0 | 1 | 1 | `00` |
| 34 | 4913 | MB::RewardTrackComponentReplicatedState | `0x143736d00` | 2 | 3 | 1 | `00` |
| 35 | 3765 | MB::ItemSkinningComponentReplicatedState | `0x14364c580` | 1 | 2 | 1 | `00` |
| 36 | 81 | MB::GatheringComponentReplicatedState | `0x144904a90` | 1 | 2 | 1 | `00` |
| 39 | 5620 | MB::MountComponentReplicatedState | `0x1438d7000` | 3 | 4 | 1 | `00` |
| 41 | 15 | MB::VitalsComponentReplicatedState | `0x14671e040` | 1 | 2 | 1 | `00` |
| 42 | 4297 | MB::StaminaComponentReplicatedState | `0x14671a230` | 0 | 1 | 1 | `00` |
| 43 | 1525 | MB::StatMultiplierTableComponentReplicatedState | `0x143fe27c0` | 2 | 3 | 1 | `00` |
| 44 | 4236 | MB::StatusEffectsComponentReplicatedState | `0x144013900` | 3 | 4 | 1 | `00` |
| 45 | 2932 | MB::CooldownTimersComponentReplicatedState | `0x1466f3c90` | 0 | 1 | 1 | `00` |
| 46 | 185 | MB::AbilityComponentReplicatedState | `0x1466ea770` | 0 | 1 | 1 | `00` |
| 47 | 1652 | MB::ManaComponentReplicatedState | `0x1467088c0` | 0 | 1 | 1 | `00` |
| 48 | 5257 | MB::ChargeComponentReplicatedState | `0x1466f13d0` | 0 | 1 | 1 | `00` |
| 49 | 3152 | MB::FactionComponentReplicatedState | `0x1430a7910` | 3 | 4 | 1 | `00` |
| 50 | 4321 | MB::WaypointsComponentReplicatedState | `0x14430bd30` | 0 | 1 | 1 | `00` |
| 51 | 1994 | Javelin::GroupsComponentReplicatedState | `0x14342f250` | 1 | 2 | 1 | `00` |
| 55 | 4176 | MB::SocialReplicatedState | `0x143ed7110` | 1 | 2 | 1 | `00` |
| 56 | 3147 | Javelin::GuildsComponentReplicatedState | `0x1434dfd30` | 1 | 2 | 1 | `00` |
| 57 | 1566 | MB::ChatReplicatedState | `0x143e71030` | 0 | 1 | 1 | `00` |
| 60 | 3786 | MB::CurrencyComponentReplicatedState | `0x1437ff0e0` | 0 | 1 | 1 | `00` |
| 61 | 1755 | MB::ContainerComponentReplicatedState | `0x1448eab40` | 0 | 1 | 1 | `00` |
| 62 | 3183 | MB::PaperdollComponentReplicatedState | `0x14670d610` | 2 | 3 | 1 | `00` |
| 65 | 911 | MB::CategoricalProgressionReplicatedState | `0x143ca3ca0` | 0 | 1 | 1 | `00` |
| 66 | 3210 | MB::AchievementComponentReplicatedState | `0x14317fec0` | 0 | 1 | 1 | `00` |
| 67 | 3681 | MB::ProgressionPointReplicatedState | `0x143cdfa30` | 0 | 1 | 1 | `00` |
| 68 | 497 | Javelin::GameEventComponentReplicatedState | `0x1431e5850` | 0 | 1 | 1 | `00` |
| 70 | 899 | MB::ProgressionComponentReplicatedState | `0x143cdf390` | 1 | 2 | 1 | `00` |
| 71 | 4878 | Javelin::MusicalPerformancePlayerComponentReplicatedState | `0x143964aa0` | 1 | 2 | 1 | `00` |
| 72 | 5485 | Javelin::SeasonsRewardsReplicatedState | `0x143da8ed0` | 0 | 1 | 1 | `00` |
| 73 | 5606 | Javelin::SeasonsRewardsTrackedStatReplicatedState | `0x143dd1460` | 0 | 1 | 1 | `00` |
| 74 | 5691 | MB::TransmogComponentReplicatedState | `0x14410fa20` | 0 | 1 | 1 | `00` |
| 76 | 1755 | MB::ContainerComponentReplicatedState | `0x1448eab40` | 0 | 1 | 1 | `00` |
| 77 | 2938 | MB::GlobalStorageComponentReplicatedState | `0x142cf3c30` | 0 | 1 | 1 | `00` |

**Observed.** Components 61 and 76 use the same class (type 1755,
`MB::ContainerComponentReplicatedState`); both share vtables `0x1483950B8`/`0x148395050`
and handlers.

**Observed.** Component 56 (`GuildsComponentReplicatedState`) has no field list in the
Azoth catalog packets. Its `Unmarshal` thunk `0x1434c9700` calls `0x1434dfd30` directly;
that function registers 26 `guild*` children, stores vtable `0x1481e3038`, and calls
add-slot once. Two slots.

**Observed.** Type 10 (component 0, the spawn card) is also read by the generic reader in
the capture (decoder `0x17b4110`, bodies 18-38 bytes). It is not an empty state.

\* ALC `+0x90` is the custom decoder `FUN_142a436b0` (old) / `FUN_142a44650` (current), not
the generic reader; see section 3. Its slot byte also selects nothing on `0x00`, but a bare
`0x00` gives the client an ALC with no children at all.

**Inferred.** The `adds` counts are exact: every `FUN_141677dd0` call in these constructors
runs once, straight-line, no loops, no helper-mediated calls (cross-call check: the only
callee among the registration functions is the base constructor). The field-name match was
unique for 55 classes; the cluster check against catalog `CreateInstance`/`Unmarshal`
addresses agrees for all of them.

**Observed.** In the capture, the retail server never sends `0x00` for any of the 56 types:
zero one-byte bodies in 178,170 fragments. The per-type first body after a spawn card is a
real snapshot, from 3 bytes (types 603, 5257) to several thousand (type 3652: ~6.7 KB,
type 2932: ~5.9 KB, type 3183: ~1.7 KB, type 3133: ~4 KB). Component 5 (PlayerTime, type
3290) is always a 26-byte body.

## 3. `ALCReplicatedState` (type 11)

**Observed.** Virtual `+0x90` on the type-11 vtable is `FUN_142a436b0` on the old build;
the address map gives `FUN_142a44650` (`0x142a44650`) on the current build. The catalog
`Unmarshal` `FUN_142a327f0` (current `FUN_142a33790`) is a wrapper that constructs the
object and calls `FUN_146160ae0`; the bundle path uses `+0x90`.

**Observed.** Grammar of `FUN_142a44650` (decompile, current build; identical shape on the
old build):

- One slot byte; bit 0 selects slot 0, bit 1 selects slot 1. No other bit is tested.
- For each selected slot: one wide prefix integer (`FUN_14087b910`, the same codec as
  `Reader::wide_prefixed` in `crates/nw-messages/src/wire.rs`). Its low bits are the child
  presence mask; the client masks to 54 bits for slot 0.
- For each set bit, in constructor order: that child's virtual `+0x30` reader.
- Slot 0 has 54 children; slot 1 has 9 (`crates/nw-messages/src/alc.rs` child table matches
  the decompiled reader list and passes all walked bodies).

**Observed.** On this capture's local player (key 16), the first type-11 body after each
spawn card is 9-25 bytes and walks cleanly (65 of 65). Slot-0 children 0, 1, 2 are set in
all 65; children 10 and 11 in ~49-53; children 4, 12, 26, 27 (the `pos10` position child)
less often. Slot 1 appears in 11 of 65 (child 0 only). Other entities' first ALC bodies
(key 5 in the same table) are larger, up to ~76 bytes with ~26 slot-0 children.

**Observed.** A body of `0x00` (no slots selected) is accepted by the decoder grammar.
It carries no children, so it carries no position.

**Observed.** The minimal body that still gives the entity a position is the one already
encoded by `AlcState::at_position` in `crates/nw-messages/src/alc.rs`: slot byte `0x01`,
wide-prefix presence mask `1 << 27` = bytes `e0 00 00 80`, then the 10-byte `pos10` child
(big-endian `f32` x, `f32` y, `u16` z). Total 15 bytes: `01 e0 00 00 80` + 10 bytes.

**Inferred.** For a newly portrayed player the client does not require a fixed set of ALC
children to parse; the decoder accepts any subset. But the retail first body always carries
at least children 0-2 and usually the movement group 10-12; the safe minimal first body is
the 15-byte `pos10` form above. Whether an ALC with no position causes a later failure is
**Unknown**; the crashes below are elsewhere.

## 4. Crash 1: `FUN_144087df0`

**Observed.** Fault: read access at `0x5f`, instruction `callq *0x60(%rax)` at
`0x144087e0a` inside `FUN_144087df0`, `rax = 0xffffffffffffffff`, `r9 = 5`. Stack:
`FUN_14167beb0` ← `FUN_14167b0a0` ← `FUN_14178ee90` (GDE `AcquireSliceAsset`) ←
`FUN_1417b6350` (GDE streamer). This run sent card + ALC only.

**Observed.** `FUN_144087df0` is the virtual `+0xb0` apply slot of vtable `0x148302158`.
The object with that vtable is allocated by `FUN_1440842d0`, which passes the debug name
`"PlayerTimeComponentClientFacet"` and stores the facet at `component + 0x80`. So the
crashing apply belongs to component index 5, `MB::PlayerTimeComponentReplicatedState`
(type 3290). That is consistent with `r9 = 5`.

**Observed.** The apply body:

```c
if ((*(state+0x7c0)->vtable + 0x60)(state+0x7c0))  *(facet_out+0xf0) = *(state+0x7d8);
if ((*(state+0x808)->vtable + 0x60)(state+0x808))  *(facet_out+0x100) = *(state+0x820);
if ((*(state+0x850)->vtable + 0x60)(state+0x850))  *(facet_out+0x110) = *(state+0x868);
```

`state+0x7c0`, `+0x808`, `+0x850` are the three embedded child objects registered by the
PlayerTime constructor `FUN_14407de10` (`startTimePoint`, `durationAtStart`,
`paidDurationAtStart`, all slot 0). In a constructed state, `state+0x7c0` is the child's
vtable pointer, never `-1`.

**Observed.** The apply runs over the entity's component-state map, not over the received
records: `FUN_14167b0a0` walks a tree (`entity+0x88`) of per-component records and calls
`FUN_14167beb0` with the component index and the record's stored state shared_ptr.
`FUN_14167beb0` looks up the component (`entity+0x160` table, index 5), loads the facet at
`component+0x80`, and calls `facet->vtable[0xb0](facet, *recordState)`.

**Observed.** `CreateInstance` for type 3290 (`0x14406F400`, old build) is a thunk:
`mov rcx,rdx; mov edx,0x898; jmp 0x147aa664c`, and `0x147aa664c` jumps into the allocator.
It returns raw memory; it does not run the state constructor. The constructor runs on the
unmarshal path (`Unmarshal` `0x14406f410` calls `FUN_14407de10`, then drives the state
through `FUN_146160ae0`). The same shape holds for the other classes (e.g. type 3147:
`CreateInstance` `0x14349f8f0` allocates `0x1d10` raw).

**Inferred.** The state argument is invalid when no type-3290 record ever unmarshalled into
this entity's component-5 slot: the record then holds the raw `CreateInstance` allocation
(or an equivalent placeholder), where `+0x7c0` is uninitialized heap data (`0xff` bytes in
this run) instead of the `startTimePoint` child vtable. The apply dereferences it as a
child object and calls through `rax = -1`. This also explains why crash 1 happens with card
+ ALC and no component records at all: the apply iterates the entity's own component map,
which is filled when the entity is created, independent of what the bundle carried.

**Inferred.** To pass this path the record for component 5 must go through unmarshal once,
which builds the real `0x898`-byte state with the three children embedded. A `0x00` body
constructs the object (the ctor runs inside unmarshal) but leaves every child unset; the
apply then calls the children's `+0x60` "is set" check on real objects, which is safe. The records
that must exist are therefore all components the client creates facets for, not only
PlayerTime; the retail server sends a real body for every one of the 56.

**Unknown.** Whether a `0x00` body for PlayerTime is enough end-to-end (the apply only reads
children that report "set", but the facet may consume `startTimePoint` later). The capture
shows the server always sends the 26-byte body with all three children set.

## 5. Crashes 2 and 3: `FUN_14172dce0`

**Observed.** Fault at `0x14172de81` inside `FUN_14172dce0`, read access at
`0x0000006c65526471`. `rax = 0x0000006c65526469` is ASCII text (`"idRel…"`) used as a
pointer, faulting on `[rax+8]`. Called from `FUN_141718de0` (per-record state handler on
the portrayal path) ← `FUN_14175dbe0` (bundle member walk).

**Observed.** `FUN_14172dce0` merges a received state into the entity's stored state for one
component. It iterates the target object's slot vector (`(param_1[3] - param_1[2]) / 800`)
and inside each slot a child table of stride `0x18`, dereferencing each child entry's
`+8` pointer and calling the child's `+0x38`/`+0x48` virtuals. The fault is exactly that
inner child-pointer load.

**Inferred.** The walked object is not a constructed state: its slot-vector or child-table
fields hold bytes of a string (ASCII), the same class of corruption as crash 1. The
entity-side record for the component held a placeholder/raw block (or a descriptor record,
whose embedded uuid/name fields are ASCII) rather than a constructed state when the merge
ran. A wrong-length read upstream that left the cursor on ASCII is a possible alternative,
but no class needs a second slot byte, so the sent `0x00` bodies each consume exactly one
byte; the desync, if any, is outside the state bodies.

**Unknown.** Which component's merge faulted, and whether the corrupt object is the merge
target (the stored placeholder) or the merge source (the just-unmarshalled record). Both
decompiled paths are reachable; the register dump does not name the component.

## 6. Consequences for the emulator

**Inferred.** Sending `0x00` for the 56 components is correct at the byte level, but it is
not sufficient: the client builds a component map for the entity at spawn and applies or
merges every entry, and an entry that never unmarshalled a real record holds a raw
allocation, not a state object. The minimum fixable set is: a real spawn card (type 10),
a parseable ALC (type 11; the 15-byte `pos10` minimum), and a record per component that
runs that class's unmarshal. A `0x00` body does run it, because the constructor executes
inside `Unmarshal` before any body bytes are consumed. The remaining risk is facets whose
apply reads fields a `0x00` body leaves unset (PlayerTime reads only "is set" flags; other
facets may do more). The reliable reference is the capture's per-type first bodies, which
are real snapshots, never `0x00`.

## Files read

- `local-data/handoff-player-components-re.md` (not kept)
- `docs/re/2026-09-06-build-25810808-address-map.md`, `docs/re/2026-09-04-m6-alc-state.md`,
  `docs/re/2026-09-06-m6-component-tables.md`, `docs/re/2026-09-04-m6-vitals-state.md`,
  `docs/re/2026-09-08-world-entry-live.md`
- `local-data/Aeternum-World-main/Catalog/uuid_to_class.json`,
  `local-data/azoth-catalog/packets/*.json`,
  `local-data/captures/open-world/NH-MSQ-10.2-Quest-61c0a783ba281885/fragments.jsonl`
  (counts, sizes, decoder RVAs only)
- `crates/nw-messages/src/{alc.rs,wire.rs,player_components.rs,slots.rs}`
- `~/steam-1063730.crash1.log`, `~/steam-1063730.crash2.log`, `~/steam-1063730.log`
- Ghidra old-build project `<re-scratch>/ghidra`, current-build project
  `<re-scratch>/ghidra-25810808`;
  scripts `PcCtors.java`, `PcCount.java`, `PcVt90.java`, `PcVref.java`, `PcDump.java`,
  `PcRec.java` in `<re-scratch>/scripts/`; decompiles in
  `<re-scratch>/scratch-pc/ctors-old/` and `<re-scratch>/scratch-pc/crash/`.
- REA `open_binary` was not called. No REA Evidence ID.
