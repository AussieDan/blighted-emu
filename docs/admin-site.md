# The admin site

`blighted-admin` is the team's web tool for the game servers: it shows the live world, acts on
players, edits the game data, and reads capture files.

## Start it

```bash
cargo build --release -p blighted-admin
target/release/blighted-admin serve
```

It reads `config/blighted-admin.toml` (another file with `--config`) and serves
http://127.0.0.1:9180. On this PC there is no login (local mode). On a server, `[access]` turns on
the login (Discord when `[access.discord]` is set), and `[access.relay]` serves the site through a
relay on another machine.

The pages read the files that `just bundle`, `just world`, `just map-tiles` and
`content-tool collision` write into `local-data/content`. Live, Live 3D and GM need the game
servers running (`[live]` in the configuration).

## The pages

The tab bar at the top opens each page. The button at its right end switches night and day.

### Live

The world map with every sim, player, bot and mob, live.

- Server standings (top right): each sim with its state and player count, then the main figures.
- The tool column (left): Selected, Events, Players, GM tools, Server and Key. One opens at a time.
- Click a player, bot or mob on the map (or a row of Players) to select it. Selected then shows it
  and the GM controls for it.
- Right-click the map to put that point in the GM mob spawn fields.
- Bots (bottom): start test bots in batches of 1, 10, 100 or 1000, or stop them all.

### Live 3D

The world as a low-poly wire model: the ground, rocks, cliffs, buildings and props, trees and
bushes, the sea, and the players and mobs near the camera.

- Trees and bushes are the tree and bush gatherables of the world file (the ones players can
  chop or pick), drawn as green cones. Their height comes from the size word of the name (Tiny 5 m
  to Huge 24 m for a tree): the game data read here has no plant size, so these heights are our
  choice. Trees that cannot be chopped are not shown.

- Follow: pick a player to follow. With no one online, the page looks at the middle of the world.
- Go to: type x (metres east) and y (metres north) to look at any place.
- Map colours on the ground: paints the map on the ground in place of the wire.
- Left-drag turns, right-drag moves, the wheel zooms, `R` puts the camera behind the player.
- The first load after a start takes several seconds: the site reads the level collision file
  (about 100 MB) once.

### GM

- Players online: everyone in the game now, on every sim. Pick one to get their controls:
  teleport, heal, god mode, kill, kick, give or take items, start a quest, set the level, give
  experience, set weapon mastery, set coins.
- Ban an account: by login name, Discord user id or Discord username. A banned account cannot log
  in.
- Admin ranks (head admin only): who may use the site. A CSR may only kick. A game master may do
  every other command and bans. A head admin may also set ranks.
- Audit log: every GM command and every admin login.

### Studio

Changes the game world with no code. Each change is one small design file in `content/`.

1. Pick a kind in the Editor: AI profile (how a mob acts), Path (a patrol route) or Spawn group
   (new mobs at points you pick).
2. Fill in the fields. For a path or a spawn group, right-click the map to add a point.
3. Save. The file shows in Design files. The game does not change yet.
4. Apply (top right). It checks every design file, writes the merged game data and restarts the
   local world. If a check fails, Problems lists it and Apply writes nothing.

Every file format is in `content/README.md`.

### Records

Find a game data record (items, weapons, armor, consumables, perks, loot tables, mobs, NPCs,
quests, quest tasks) and change its fields. A change is saved as a design file; Apply on Studio
puts it in the game. Mobs with no loot lists the mobs that drop nothing.

### Scripts

Block scripts: a trigger (a player enters an area, a mob dies, a timer), conditions and actions
(spawn mobs, give an item, start a quest). Click a block in the list to add it, click an output dot
then a block to link them, click a link to remove it. Save writes the script as a design file. The
run log shows the blocks a script passed on the local world.

### To-do

The team's task list (`content/todo.toml`). Add a task or import a GitHub issue, set its state and
who has it, and link the design files that belong to it before you commit.

### Map

The world map with its layers: map tiles, relief, or both. Mob spawns, Gatherables and Landmarks
show where those points are; zoomed out, each 256 m square is coloured by how many it holds, and
zoomed in, each point shows. Click a mob spawn to see its design key (the id a `spawn_point` design
file uses).

### Captures

Reads capture files (`ledger.bin`) from `local-data/captures`: the session's paths on the map, a
time slider, the message types, each message, its bytes, where the decoder stopped, and a compare
of two sessions. A site behind a relay serves no capture data.
