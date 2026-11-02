# Design files

This folder holds the team's changes to the game world, one TOML file per record. The game data
itself is never stored here. It comes from your own New World install. `just design` merges these
files over the client data and writes the files the servers read. The Apply button on the Studio
page of the admin site (`/studio`) does the same.

`todo.toml` at the root of this folder is the team's to-do list. It is not a design file, and
the loader skips it.

## A file

```toml
schema = 1
kind = "loot_table"        # what the file changes (table below)
action = "override"        # override | add | disable
id = "LT_EXAMPLE"          # the record id; also the file name: LT_EXAMPLE.toml
max_roll = 50              # the fields to set
```

- `override` sets the named fields of the client record with this id. The other fields keep
  their client values.
- `add` makes a new record. To start from a client record, set `base = "<id>"` and name only
  the fields you change. Without `base`, the file must hold every required field of the row.
- `disable` removes the client record. It takes no fields.

Only `add` takes `base`. Two files of one kind (and one level) cannot use the same id.

Field names are the snake_case names of the row structs in `crates/nw-content/src/generated/`.
Do not set the id field of the row: `id` gives it. Do not copy client values into a file. Name
only what you change.

## Kinds and folders

| `kind` | Folder | Client data | Id |
|---|---|---|---|
| `item` | `items/` | `MasterItemDefinitions` | `ItemID` |
| `weapon` | `items/weapons/` | `WeaponItemDefinitions` | `WeaponID` |
| `armor` | `items/armor/` | `ArmorItemDefinitions` | `WeaponID` |
| `consumable` | `items/consumables/` | `ConsumableItemDefinitions` | `ConsumableID` |
| `perk` | `perks/` | `PerkData` | `PerkID` |
| `loot_table` | `loot/tables/` | `LootTablesData` | `LootTableID` |
| `mob` | `mobs/` | `VitalsBaseData` (also its `loot_table_id`, `loot_drop_chance`, `loot_tags`) | `VitalsID` |
| `npc` | `npcs/types/` | `NPCData` | `NPCId` |
| `objective` | `quests/` | `Objectives` | `ObjectiveID` |
| `objective_task` | `quests/tasks/` | `ObjectiveTasks` | `TaskID` |
| `spawn_point` | `spawns/<map>/` | creature spawn points of a world file | point key |
| `npc_point` | `npcs/<map>/` | NPC points of a world file | point key |
| `ai_profile` | `ai/` | (own) a behaviour and brain numbers | profile id |
| `path` | `paths/<map>/` | (own) a patrol path of one level | path id |
| `spawn_group` | `spawns/<map>/` | (own) new mob spawns of one level | group id |
| `script` | `scripts/<map>/` | (own) a visual block script of one level | script id |
| `event` | `events/<map>/` | (own) waves of mobs a GM starts | event id |

A kind with `<map>` in its folder names its level with `map`, and the file goes in the folder
of that level. The other kinds take no `map`.

## Spawn and NPC points

A retail point has no id of its own, so it gets a point key: `<type id>_<x>_<y>_<z>`. Each
coordinate is rounded to 0.1 m, and a second or later point with the same key gets `_2`, `_3`,
and so on. To find the key of a mob spawn, turn on "Mob spawns" on the Map page of the admin
site, zoom in, and click the spawn. The status line then shows its design key. NPC points are
not on the map yet.

```toml
schema = 1
kind = "spawn_point"
action = "override"
id = "123456789_100.0_200.0_30.5"
map = "newworld_vitaeeterna"
level = 30
```

A `spawn_point` file takes `vitals_id` and `category_id` (text ids), `level` (1 to 65) and
`position` (`[x, y, z]` in metres). An `npc_point` file takes `npc_id`, `faction_type` (0 to
255) and `position`. You can change or disable a point, but you cannot add one. New spawns come
from spawn groups.

## AI profiles, paths and spawn groups

These three kinds have no client record, so their action is always `add`.

```toml
# ai/gate_guard.toml: every number is optional.
schema = 1
kind = "ai_profile"
action = "add"
id = "gate_guard"
behaviour = "guard"        # wander (default) | guard | patrol | idle
sight_m = 25.0
speed_mps = 4.0
respawn_ms = 60000
```

- `wander`: walks near its spawn and attacks a player in sight.
- `guard`: stands at its spawn, attacks a player in sight, then goes back.
- `patrol`: walks its `path`, attacks a player in sight, then goes back to where it left the path.
- `idle`: stands at its spawn and fights only a player who hits it.

The other numbers are `wander_radius_m`, `leash_radius_m`, `attack_interval_ms`,
`wake_radius_m`, `wander_pause_ms` and `attack_reach_m`. A number that is not set keeps the
value of the mob's own behaviour tree. When `respawn_ms` is not set, the sim uses its `[combat]`
value `mob_respawn_ms`. The check accepts `wake_radius_m`, `wander_pause_ms` and
`attack_reach_m`, but the sim does not use them yet. It logs a warning and leaves them out.

```toml
# paths/newworld_vitaeeterna/gate_walk.toml
schema = 1
kind = "path"
action = "add"
id = "gate_walk"
map = "newworld_vitaeeterna"
points = [[100.0, 200.0, 30.0], [140.0, 200.0, 30.0]]
waits_ms = [3000, 0]       # optional: the wait at each point
mode = "back_and_forth"    # loop (default) | back_and_forth | once

# spawns/newworld_vitaeeterna/gate_wolves.toml
schema = 1
kind = "spawn_group"
action = "add"
id = "gate_wolves"
map = "newworld_vitaeeterna"
vitals = "Wolf_Alpha"      # a VitalsBaseData id
level = 20
points = [[100.0, 205.0, 30.0], [102.0, 207.0, 30.0]]
ai_profile = "gate_guard"  # optional
path = "gate_walk"         # optional: a path of the same level
```

A `spawn_point` file can also set `ai_profile` and `path`. The profile and the path must be
`add` files of this folder, and the path must be of the same level. The spawns of a spawn group
get the keys `<group id>_1`, `<group id>_2`, and so on.

## Scripts and events

Make scripts with the script editor (`/scripts`). It writes the form below. A script has nodes
and links. A node has an `id`, a `block`, the editor place `x` and `y`, and the values of its
block. A link goes from an output of one node to another node. A condition has the outputs
`true` and `false`. Every other block has the output `next`.

```toml
# scripts/newworld_vitaeeterna/gate_greeting.toml
schema = 1
kind = "script"
action = "add"
id = "gate_greeting"
map = "newworld_vitaeeterna"

[[nodes]]
id = "enter"
block = "player_enters_area"
x = 40
y = 60
center = [100.0, 200.0, 30.0]
radius_m = 15.0

[[nodes]]
id = "gift"
block = "give_item"
x = 260
y = 60
item = "FlintT1"

[[links]]
from = "enter"
output = "next"
to = "gift"
```

The blocks are:

- Triggers: `player_enters_area`, `player_leaves_area`, `mob_dies`, `timer`, `quest_done`,
  `event_starts`.
- Conditions: `random_chance`, `has_item`, `quest_state`, `flag_is`, `mobs_alive`.
- Actions: `spawn_mobs`, `despawn_mobs`, `give_item`, `start_quest`, `set_flag`, `start_event`.
- Flow: `wait`.

The check refuses a script with any of these problems:

- It has no trigger.
- Two nodes use the same id, or a node names an unknown block.
- A value is missing, has the wrong type, or is not a value of the block.
- A link names an unknown node or an output that the block does not have.
- A link goes into a trigger, or nothing links into a node that is not a trigger.
- A loop has no `wait`.
- The `event` of an `event_starts` or `start_event` node is not an event file of the same level.

```toml
# events/newworld_vitaeeterna/town_invasion.toml
schema = 1
kind = "event"
action = "add"
id = "town_invasion"
map = "newworld_vitaeeterna"
end_after_ms = 600000      # optional
end_when_cleared = true    # optional: ends when every wave is dead

[[waves]]
vitals = "Wolf_Alpha"
level = 20
count = 5
delay_ms = 0               # optional
offset = [0.0, 0.0, 0.0]   # optional: from the point the GM picks
```

An event needs at least one wave. A wave takes `level` 1 to 65 and `count` 1 to 50.

The scripts and events go into `design-<build>.json` beside the merged files. The sims read it
at start.

## Checks

- `just check` runs on any PC and needs no game. Its tests check the files of this folder: the
  file format, the folders, the field names and the values.
- `just check-design` needs `just bundle` and `just world` first. It also checks every id
  against the client data. Apply on the Studio page runs this check first and writes nothing
  when it finds an error.
