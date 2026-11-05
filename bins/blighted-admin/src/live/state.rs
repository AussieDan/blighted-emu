// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live state of the running servers (admin tools spec section 5.2): sims from their
//! heartbeats, the cell table, the newest complete snapshot of each sim, the metrics of every
//! metrics page, CPU per program, the bot total and the event log. [`LiveState::view`] is what
//! the page gets.

use std::collections::{BTreeMap, VecDeque};

use nw_bus::{
    BusId, CellTable, MOB_STATES, ScriptLog, ScriptLogLine, SimHeartbeat, SimSnapshot, SnapMob,
    SnapPlayer,
};
use nw_common::prometheus::Scrape;
use rep_gateway::server::{
    METRIC_BYTES_IN, METRIC_BYTES_OUT, METRIC_DATAGRAMS_IN, METRIC_DATAGRAMS_OUT,
};
use serde::Serialize;
use sim::server::{METRIC_BUNDLE_MEMBERS, METRIC_TICK_SECONDS, ONE_TICK_SECONDS};

use crate::config::SimSilence;

/// Events the log keeps.
pub const EVENTS_KEPT: usize = 200;
/// Script run log lines kept for the script editor.
pub const SCRIPT_LINES_KEPT: usize = 500;
/// The tick quantile of the p99 tile.
const P99: f64 = 0.99;
const MS_PER_SECOND: f64 = 1_000.0;
/// The position of the asleep state in the mob counts.
const ASLEEP: usize = 0;
const SUM_SUFFIX: &str = "_sum";
const COUNT_SUFFIX: &str = "_count";

/// The tiles row (each `None` is shown as "Unknown").
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Tiles {
    /// Sims with a recent heartbeat.
    pub sims: u32,
    /// Players of those sims, from their heartbeats.
    pub players: u32,
    /// Bots of the running batches.
    pub bots: u32,
    /// CPU per program, in % of one core.
    pub cpu: Vec<(String, Option<f64>)>,
    /// Average tick time over the last scrape interval, every page together.
    pub tick_avg_ms: Option<f64>,
    /// 99th percentile tick time (a bucket bound) over the last scrape interval.
    pub tick_p99_ms: Option<f64>,
    /// Gateway datagrams received per second.
    pub datagrams_in_s: Option<f64>,
    /// Gateway datagrams sent per second.
    pub datagrams_out_s: Option<f64>,
    /// Gateway bytes received per second.
    pub bytes_in_s: Option<f64>,
    /// Gateway bytes sent per second.
    pub bytes_out_s: Option<f64>,
    /// Bundle members per second, every sim together.
    pub members_s: Option<f64>,
    /// Awake mobs of the shown snapshots.
    pub mobs_awake: Option<u64>,
    /// Asleep mobs of the shown snapshots.
    pub mobs_asleep: Option<u64>,
}

/// One sim with a recent heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SimView {
    /// The sim.
    pub id: String,
    /// Its players.
    pub players: u32,
    /// Players it takes at most.
    pub max_players: u32,
}

/// One metrics page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricsView {
    /// The page.
    pub url: String,
    /// The last scrape worked.
    pub up: bool,
    /// Its average tick time over the last interval.
    pub tick_avg_ms: Option<f64>,
    /// Its 99th percentile tick time over the last interval.
    pub tick_p99_ms: Option<f64>,
}

/// One player of a shown snapshot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlayerView {
    /// The entity number.
    pub entity: u64,
    /// The character's name.
    pub name: String,
    /// The name starts with a bot prefix.
    pub bot: bool,
    /// Its sim.
    pub sim: String,
    /// East.
    pub x: f32,
    /// North.
    pub y: f32,
    /// Height.
    pub z: f32,
    /// Dead.
    pub dead: bool,
    /// Frozen for a handoff.
    pub frozen: bool,
    /// Current health.
    pub health: u32,
    /// Max health.
    pub max_health: u32,
    /// The entity number the player is attacking, when in a fight.
    pub target_entity: Option<u64>,
}

/// One awake mob of a shown snapshot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MobView {
    /// The entity number.
    pub entity: u64,
    /// Its sim.
    pub sim: String,
    /// East.
    pub x: f32,
    /// North.
    pub y: f32,
    /// Up.
    pub z: f32,
    /// State, in mob state order.
    pub state: u8,
    /// Current health.
    pub health: u32,
    /// Max health.
    pub max_health: u32,
    /// The entity number the mob is attacking, when in a fight.
    pub target_entity: Option<u64>,
}

/// One event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventView {
    /// Unix milliseconds.
    pub time_ms: u64,
    /// What happened.
    pub text: String,
}

/// What the page gets.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LiveView {
    /// The tiles row.
    pub tiles: Tiles,
    /// The sims.
    pub sims: Vec<SimView>,
    /// The metrics pages.
    pub metrics: Vec<MetricsView>,
    /// The newest cell table.
    pub table: Option<CellTable>,
    /// Players of the shown snapshots.
    pub players: Vec<PlayerView>,
    /// Awake mobs of the shown snapshots.
    pub mobs: Vec<MobView>,
    /// The newest events, oldest first.
    pub events: Vec<EventView>,
    /// One sim tick at 30 Hz: a tick p99 above it is a slow tick.
    pub tick_budget_ms: f64,
}

#[derive(Debug, Clone)]
struct SimEntry {
    started_at_ms: u64,
    players: u32,
    max_players: u32,
    last_seen_ms: u64,
}

/// The newest complete snapshot of one sim, and the parts of a newer one.
#[derive(Debug, Clone, Default)]
struct Snapshots {
    shown_ms: Option<u64>,
    players: Vec<SnapPlayer>,
    mobs: Vec<SnapMob>,
    counts: Option<[u32; MOB_STATES]>,
    pending_ms: Option<u64>,
    pending: Vec<Option<SimSnapshot>>,
}

/// The last scrape of one page and its change from the scrape before.
#[derive(Debug, Clone, Default)]
struct Page {
    up: bool,
    last: Option<(u64, Scrape)>,
    /// Seconds between the two scrapes, and the change.
    delta: Option<(f64, Scrape)>,
}

/// The live state.
#[derive(Debug, Default)]
pub struct LiveState {
    sims: BTreeMap<BusId, SimEntry>,
    table: Option<CellTable>,
    snapshots: BTreeMap<BusId, Snapshots>,
    pages: BTreeMap<String, Page>,
    cpu: Vec<(String, Option<f64>)>,
    bots: u32,
    events: VecDeque<EventView>,
    bot_prefixes: Vec<String>,
    /// A sim silent for longer than this has left.
    silence: SimSilence,
    script_lines: VecDeque<ScriptLineView>,
}

/// One script run log line with the sim that ran it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScriptLineView {
    /// The sim.
    pub sim: String,
    /// The line.
    #[serde(flatten)]
    pub line: ScriptLogLine,
}

impl LiveState {
    /// The run log lines of a sim; the newest [`SCRIPT_LINES_KEPT`] stay.
    pub fn script_log(&mut self, log: ScriptLog) {
        for line in log.lines {
            if self.script_lines.len() == SCRIPT_LINES_KEPT {
                self.script_lines.pop_front();
            }
            self.script_lines.push_back(ScriptLineView {
                sim: log.sim.to_string(),
                line,
            });
        }
    }

    /// The kept run log lines, oldest first.
    pub fn script_lines(&self) -> Vec<ScriptLineView> {
        self.script_lines.iter().cloned().collect()
    }

    /// An empty state that marks players whose names start with one of `bot_prefixes` as bots,
    /// and drops a sim after `silence` without a heartbeat.
    pub fn new(bot_prefixes: Vec<String>, silence: SimSilence) -> Self {
        Self {
            bot_prefixes,
            silence,
            ..Self::default()
        }
    }

    fn is_bot(&self, name: &str) -> bool {
        self.bot_prefixes
            .iter()
            .any(|prefix| name.starts_with(prefix.as_str()))
    }

    /// A sim heartbeat at `now_ms`; a new sim or a new start time is an event.
    pub fn heartbeat(&mut self, beat: SimHeartbeat, now_ms: u64) {
        let entry = SimEntry {
            started_at_ms: beat.started_at_ms,
            players: beat.players,
            max_players: beat.max_players,
            last_seen_ms: now_ms,
        };
        match self.sims.insert(beat.id.clone(), entry) {
            None => self.event(format!("sim {} joined", beat.id), now_ms),
            Some(old) if old.started_at_ms != beat.started_at_ms => {
                self.snapshots.remove(&beat.id);
                self.event(format!("sim {} restarted", beat.id), now_ms);
            }
            Some(_) => {}
        }
    }

    /// The ids of the sims with a recent heartbeat, sorted.
    pub fn sim_ids(&self) -> Vec<String> {
        self.sims.keys().map(ToString::to_string).collect()
    }

    /// How many sims of `ids` have a heartbeat that says they started after `after_ms`.
    pub fn started_after(&self, ids: &[String], after_ms: u64) -> usize {
        self.sims
            .iter()
            .filter(|(id, sim)| sim.started_at_ms > after_ms && ids.contains(&id.to_string()))
            .count()
    }

    /// How many sims have a heartbeat that says they started after `after_ms`.
    pub fn started_since(&self, after_ms: u64) -> usize {
        self.sims
            .values()
            .filter(|sim| sim.started_at_ms > after_ms)
            .count()
    }

    /// The sim whose newest snapshot holds the player with entity number `entity`.
    pub fn sim_of_player(&self, entity: u64) -> Option<BusId> {
        self.snapshots
            .iter()
            .find(|(_, snapshots)| {
                snapshots
                    .players
                    .iter()
                    .any(|player| player.entity == entity)
            })
            .map(|(sim, _)| sim.clone())
    }

    /// The sim whose newest snapshot holds the awake mob `entity`.
    pub fn sim_of_mob(&self, entity: u64) -> Option<BusId> {
        self.snapshots
            .iter()
            .find(|(_, snapshots)| snapshots.mobs.iter().any(|mob| mob.entity == entity))
            .map(|(sim, _)| sim.clone())
    }

    /// Where the player `entity` is in its sim's newest snapshot.
    pub fn player_position(&self, entity: u64) -> Option<[f32; 3]> {
        self.snapshots
            .values()
            .flat_map(|snapshots| &snapshots.players)
            .find(|player| player.entity == entity)
            .map(|player| [player.x, player.y, player.z])
    }

    /// The sim that owns the cell of (`x`, `y`) in the newest cell table.
    pub fn sim_at(&self, x: f32, y: f32) -> Option<BusId> {
        let table = self.table.as_ref()?;
        if !(x >= 0.0 && y >= 0.0 && table.cell_size_m > 0.0) {
            return None;
        }
        let column = (x / table.cell_size_m) as u32;
        let row = (y / table.cell_size_m) as u32;
        if column >= table.columns || row >= table.rows {
            return None;
        }
        let index = row as usize * table.columns as usize + column as usize;
        let slot = (*table.owners.get(index)?)?;
        table.sims.get(usize::from(slot)).map(|sim| sim.id.clone())
    }

    /// The director's cell table; a new version is an event.
    pub fn table(&mut self, table: CellTable, now_ms: u64) {
        let version = table.version;
        let changed = self.table.as_ref().is_none_or(|old| old.version != version);
        self.table = Some(table);
        if changed {
            self.event(format!("cell table version {version}"), now_ms);
        }
    }

    /// One part of a sim snapshot. A snapshot is shown when all its parts arrived; a part older
    /// than the shown snapshot or than the parts waiting is dropped.
    pub fn snapshot_part(&mut self, part: SimSnapshot) {
        let entry = self.snapshots.entry(part.sim.clone()).or_default();
        let time_ms = part.time_ms;
        if entry.shown_ms.is_some_and(|shown| time_ms <= shown)
            || entry.pending_ms.is_some_and(|pending| time_ms < pending)
        {
            return;
        }
        if entry.pending_ms != Some(time_ms) {
            entry.pending_ms = Some(time_ms);
            entry.pending = vec![None; usize::from(part.parts.max(1))];
        }
        let index = usize::from(part.part);
        let Some(slot) = entry.pending.get_mut(index) else {
            return;
        };
        *slot = Some(part);
        if entry.pending.iter().all(Option::is_some) {
            let parts: Vec<SimSnapshot> = entry.pending.drain(..).flatten().collect();
            entry.pending_ms = None;
            entry.shown_ms = Some(time_ms);
            entry.counts = parts.iter().find_map(|part| part.mob_counts);
            entry.players = parts.iter().flat_map(|part| part.players.clone()).collect();
            entry.mobs = parts.into_iter().flat_map(|part| part.awake_mobs).collect();
        }
    }

    /// One scrape of `url` at `now_ms`, or why it failed.
    pub fn scrape(&mut self, url: &str, result: Result<Scrape, String>, now_ms: u64) {
        let page = self.pages.entry(url.to_owned()).or_default();
        match result {
            Ok(scrape) => {
                page.delta = page.last.as_ref().and_then(|(at, last)| {
                    let seconds = now_ms.saturating_sub(*at) as f64 / MS_PER_SECOND;
                    (seconds > 0.0).then(|| (seconds, scrape.delta(last)))
                });
                page.last = Some((now_ms, scrape));
                page.up = true;
            }
            Err(_) => {
                page.up = false;
                page.delta = None;
            }
        }
    }

    /// CPU per program from the newest sample.
    pub fn cpu(&mut self, per_name: Vec<(String, Option<f64>)>) {
        self.cpu = per_name;
    }

    /// Adds an event; the log keeps the newest [`EVENTS_KEPT`].
    pub fn event(&mut self, text: String, now_ms: u64) {
        if self.events.len() == EVENTS_KEPT {
            self.events.pop_front();
        }
        self.events.push_back(EventView {
            time_ms: now_ms,
            text,
        });
    }

    /// The bots of the running batches.
    pub fn bots(&mut self, running: u32) {
        self.bots = running;
    }

    /// Sims silent for longer than the heartbeat silence leave, with one event each.
    pub fn expire(&mut self, now_ms: u64) {
        let silent_ms = self.silence.ms;
        let silent: Vec<BusId> = self
            .sims
            .iter()
            .filter(|(_, sim)| now_ms.saturating_sub(sim.last_seen_ms) > silent_ms)
            .map(|(id, _)| id.clone())
            .collect();
        for id in silent {
            self.sims.remove(&id);
            self.snapshots.remove(&id);
            self.event(format!("sim {id} left"), now_ms);
        }
    }

    /// The view for the page.
    pub fn view(&self) -> LiveView {
        let deltas: Vec<&(f64, Scrape)> = self
            .pages
            .values()
            .filter(|page| page.up)
            .filter_map(|page| page.delta.as_ref())
            .collect();
        let rate = |metric: &str| {
            (!deltas.is_empty()).then(|| {
                deltas
                    .iter()
                    .map(|(seconds, delta)| delta.total(metric, None) / seconds)
                    .sum()
            })
        };
        let mut merged = Scrape::default();
        for (_, delta) in &deltas {
            merged.add(delta);
        }
        let (tick_avg_ms, tick_p99_ms) = ticks(&merged);
        let counts = self
            .snapshots
            .values()
            .filter_map(|snapshots| snapshots.counts)
            .fold(None, |sum: Option<[u64; MOB_STATES]>, counts| {
                let mut sum = sum.unwrap_or_default();
                for (total, count) in sum.iter_mut().zip(counts) {
                    *total += u64::from(count);
                }
                Some(sum)
            });
        let tiles = Tiles {
            sims: u32::try_from(self.sims.len()).unwrap_or(u32::MAX),
            players: self.sims.values().map(|sim| sim.players).sum(),
            bots: self.bots,
            cpu: self.cpu.clone(),
            tick_avg_ms,
            tick_p99_ms,
            datagrams_in_s: rate(METRIC_DATAGRAMS_IN),
            datagrams_out_s: rate(METRIC_DATAGRAMS_OUT),
            bytes_in_s: rate(METRIC_BYTES_IN),
            bytes_out_s: rate(METRIC_BYTES_OUT),
            members_s: rate(METRIC_BUNDLE_MEMBERS),
            mobs_awake: counts.map(|counts| counts.iter().skip(ASLEEP + 1).sum()),
            mobs_asleep: counts.map(|counts| counts[ASLEEP]),
        };
        LiveView {
            tiles,
            sims: self
                .sims
                .iter()
                .map(|(id, sim)| SimView {
                    id: id.to_string(),
                    players: sim.players,
                    max_players: sim.max_players,
                })
                .collect(),
            metrics: self
                .pages
                .iter()
                .map(|(url, page)| {
                    let (tick_avg_ms, tick_p99_ms) = page
                        .delta
                        .as_ref()
                        .filter(|_| page.up)
                        .map_or((None, None), |(_, delta)| ticks(delta));
                    MetricsView {
                        url: url.clone(),
                        up: page.up,
                        tick_avg_ms,
                        tick_p99_ms,
                    }
                })
                .collect(),
            table: self.table.clone(),
            players: self
                .snapshots
                .iter()
                .flat_map(|(sim, snapshots)| {
                    snapshots.players.iter().map(move |player| PlayerView {
                        entity: player.entity,
                        name: player.name.clone(),
                        bot: self.is_bot(&player.name),
                        sim: sim.to_string(),
                        x: player.x,
                        y: player.y,
                        z: player.z,
                        dead: player.dead,
                        frozen: player.frozen,
                        health: player.health,
                        max_health: player.max_health,
                        target_entity: player.target_entity,
                    })
                })
                .collect(),
            mobs: self
                .snapshots
                .iter()
                .flat_map(|(sim, snapshots)| {
                    snapshots.mobs.iter().map(move |mob| MobView {
                        entity: mob.entity,
                        sim: sim.to_string(),
                        x: mob.x,
                        y: mob.y,
                        z: mob.z,
                        state: mob.state,
                        health: mob.health,
                        max_health: mob.max_health,
                        target_entity: mob.target_entity,
                    })
                })
                .collect(),
            events: self.events.iter().cloned().collect(),
            tick_budget_ms: ONE_TICK_SECONDS * MS_PER_SECOND,
        }
    }

    /// The view for the page, with players and mobs filtered to those inside `radius_m` of the
    /// `focus` player. `None` focus returns the unfiltered view. A `focus` id that is not a known
    /// player returns a view with empty `players` and `mobs` and every other field unchanged.
    pub fn view_focused(&self, focus: Option<u64>, radius_m: f32) -> LiveView {
        let mut view = self.view();
        let Some(focus) = focus else {
            return view;
        };
        let Some([cx, cy, _cz]) = self.player_position(focus) else {
            view.players.clear();
            view.mobs.clear();
            return view;
        };
        let radius_sq = radius_m * radius_m;
        view.players
            .retain(|p| squared_horizontal_distance(cx, cy, p.x, p.y) <= radius_sq);
        view.mobs
            .retain(|m| squared_horizontal_distance(cx, cy, m.x, m.y) <= radius_sq);
        view
    }
}

/// The sim's render radius in metres: how far around a player other players and mobs show on
/// the live 3D view. Matches the retail client's awareness radius for players and mobs.
pub const RENDER_RADIUS_M: f32 = 150.0;

/// The squared horizontal distance between two points, in square metres.
fn squared_horizontal_distance(x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let dx = x1 - x2;
    let dy = y1 - y2;
    dx * dx + dy * dy
}

/// The average and the 99th percentile tick time of a scrape change, in milliseconds; `None`
/// without ticks (an infinite bucket bound has no value either).
fn ticks(delta: &Scrape) -> (Option<f64>, Option<f64>) {
    let count = delta.total(&format!("{METRIC_TICK_SECONDS}{COUNT_SUFFIX}"), None);
    if count <= 0.0 {
        return (None, None);
    }
    let sum = delta.total(&format!("{METRIC_TICK_SECONDS}{SUM_SUFFIX}"), None);
    let p99 = delta
        .quantile(METRIC_TICK_SECONDS, P99)
        .filter(|bound| bound.is_finite())
        .map(|bound| bound * MS_PER_SECOND);
    (Some(sum / count * MS_PER_SECOND), p99)
}

#[cfg(test)]
mod tests {
    use nw_bus::SimSlot;

    use super::*;

    const NOW: u64 = 1_000_000;

    #[test]
    fn script_lines_keep_the_newest() {
        let mut state = LiveState::default();
        let line = |node: usize| ScriptLogLine {
            time_ms: 1,
            script: "s".to_owned(),
            node: node.to_string(),
            block: "wait".to_owned(),
            player: None,
            note: String::new(),
        };
        state.script_log(ScriptLog {
            sim: BusId::try_from("sim-1".to_owned()).expect("id"),
            lines: (0..=SCRIPT_LINES_KEPT).map(line).collect(),
        });
        let lines = state.script_lines();
        assert_eq!(lines.len(), SCRIPT_LINES_KEPT);
        assert_eq!(lines[0].line.node, "1", "the oldest was dropped");
        assert_eq!(lines[0].sim, "sim-1");
    }
    const URL: &str = "http://127.0.0.1:9102/metrics";
    const OTHER_URL: &str = "http://127.0.0.1:9104/metrics";
    const SECOND_MS: u64 = 1_000;

    fn sim(name: &str) -> BusId {
        BusId::try_from(name.to_owned()).expect("id")
    }

    fn beat(name: &str, started_at_ms: u64, players: u32) -> SimHeartbeat {
        SimHeartbeat {
            id: sim(name),
            started_at_ms,
            players,
            max_players: 1000,
            level: "newworld_vitaeeterna".to_owned(),
            instance: None,
        }
    }

    fn player(entity: u64) -> SnapPlayer {
        SnapPlayer {
            entity,
            name: format!("Player{entity}"),
            x: 1.0,
            y: 2.0,
            z: 3.0,
            dead: false,
            frozen: false,
            health: 0,
            max_health: 0,
            target_entity: None,
        }
    }

    fn mob(entity: u64) -> SnapMob {
        SnapMob {
            entity,
            x: 1.0,
            y: 2.0,
            z: 3.0,
            state: 2,
            health: 10,
            max_health: 20,
            target_entity: None,
        }
    }

    fn part(time_ms: u64, part: u16, parts: u16, players: &[u64]) -> SimSnapshot {
        SimSnapshot {
            sim: sim("sim-a"),
            time_ms,
            part,
            parts,
            players: players.iter().copied().map(player).collect(),
            awake_mobs: vec![mob(100 + u64::from(part))],
            mob_counts: (part == 0).then_some([7, 1, 1, 0, 0, 0]),
        }
    }

    fn texts(view: &LiveView) -> Vec<&str> {
        view.events
            .iter()
            .map(|event| event.text.as_str())
            .collect()
    }

    #[test]
    fn the_sim_of_a_player_comes_from_its_snapshot() {
        let mut state = LiveState::default();
        state.snapshot_part(part(NOW, 0, 1, &[5, 6]));
        assert_eq!(
            state.sim_of_player(6).map(|sim| sim.to_string()),
            Some("sim-a".to_owned())
        );
        assert_eq!(state.sim_of_player(7), None);
    }

    #[test]
    fn started_after_counts_only_newer_starts() {
        let mut state = LiveState::default();
        state.heartbeat(beat("sim-a", 5, 0), NOW);
        state.heartbeat(beat("sim-b", 20, 0), NOW);
        assert_eq!(state.sim_ids(), ["sim-a", "sim-b"]);
        let ids = state.sim_ids();
        assert_eq!(state.started_after(&ids, 10), 1);
        state.heartbeat(beat("sim-a", 30, 0), NOW);
        assert_eq!(state.started_after(&ids, 10), 2);
        assert_eq!(state.started_after(&["sim-gone".to_owned()], 10), 0);
    }

    #[test]
    fn a_snapshot_shows_when_all_parts_arrived() {
        let mut state = LiveState::default();
        state.snapshot_part(part(NOW, 0, 2, &[1]));
        assert!(state.view().players.is_empty(), "one part of two");
        state.snapshot_part(part(NOW, 1, 2, &[2]));
        let view = state.view();
        assert_eq!(view.players.len(), 2);
        assert_eq!(view.mobs.len(), 2);
        assert_eq!(view.tiles.mobs_asleep, Some(7));
        assert_eq!(view.tiles.mobs_awake, Some(2));
    }

    #[test]
    fn an_old_part_after_a_new_snapshot_is_dropped() {
        let mut state = LiveState::default();
        state.snapshot_part(part(NOW, 0, 1, &[1]));
        state.snapshot_part(part(NOW + 100, 0, 2, &[2]));
        state.snapshot_part(part(NOW, 1, 2, &[3]));
        let shown: Vec<u64> = state.view().players.iter().map(|p| p.entity).collect();
        assert_eq!(
            shown,
            vec![1],
            "the older part did not complete the newer one"
        );
        state.snapshot_part(part(NOW - 100, 0, 1, &[4]));
        let shown: Vec<u64> = state.view().players.iter().map(|p| p.entity).collect();
        assert_eq!(
            shown,
            vec![1],
            "an older snapshot never replaces a newer one"
        );
    }

    #[test]
    fn the_view_gives_the_tick_budget() {
        const BUDGET_MS: f64 = 33.0;
        const TOLERANCE_MS: f64 = 1e-9;
        assert!((LiveState::default().view().tick_budget_ms - BUDGET_MS).abs() < TOLERANCE_MS);
    }

    #[test]
    fn players_with_a_bot_prefix_are_bots() {
        let mut state = LiveState::new(
            vec!["Adminbot".to_owned(), "Swarm".to_owned()],
            SimSilence::default(),
        );
        let mut snapshot = part(NOW, 0, 1, &[1, 2, 3]);
        snapshot.players[0].name = "Adminbotab".to_owned();
        snapshot.players[1].name = "Swarmxyz".to_owned();
        snapshot.players[2].name = "Ada".to_owned();
        state.snapshot_part(snapshot);
        let view = state.view();
        let marks: Vec<(&str, bool)> = view
            .players
            .iter()
            .map(|player| (player.name.as_str(), player.bot))
            .collect();
        assert_eq!(
            marks,
            vec![("Adminbotab", true), ("Swarmxyz", true), ("Ada", false)]
        );
    }

    #[test]
    fn a_silent_sim_leaves_with_one_event() {
        const INTERVAL_MS: u64 = 500;
        const MISSES: u32 = 4;
        let silence = SimSilence::of(INTERVAL_MS, MISSES);
        let mut state = LiveState::new(Vec::new(), silence);
        state.heartbeat(beat("sim-a", 1, 5), NOW);
        state.snapshot_part(part(NOW, 0, 1, &[1]));
        state.expire(NOW + silence.ms);
        assert_eq!(state.view().sims.len(), 1, "four misses are not yet silent");
        state.expire(NOW + silence.ms + 1);
        state.expire(NOW + silence.ms + 2);
        let view = state.view();
        assert!(view.sims.is_empty() && view.players.is_empty());
        let left = texts(&view)
            .iter()
            .filter(|text| text.contains("left"))
            .count();
        assert_eq!(left, 1);
    }

    #[test]
    fn a_restarted_sim_is_one_sim() {
        let mut state = LiveState::default();
        state.heartbeat(beat("sim-a", 1, 5), NOW);
        state.heartbeat(beat("sim-a", 2, 0), NOW + SECOND_MS);
        let view = state.view();
        assert_eq!(view.sims.len(), 1);
        assert_eq!(view.tiles.sims, 1);
        assert_eq!(view.tiles.players, 0);
        assert!(texts(&view).iter().any(|text| text.contains("restarted")));
    }

    fn tick_text(sum: f64, count: u64, fast: u64) -> String {
        format!(
            "sim_tick_seconds_bucket{{le=\"0.005\"}} {fast}\n\
             sim_tick_seconds_bucket{{le=\"0.033\"}} {count}\n\
             sim_tick_seconds_bucket{{le=\"+Inf\"}} {count}\n\
             sim_tick_seconds_sum {sum}\n\
             sim_tick_seconds_count {count}\n"
        )
    }

    #[test]
    fn tick_tiles_come_from_histogram_deltas() {
        let mut state = LiveState::default();
        state.scrape(URL, Ok(Scrape::parse(&tick_text(10.0, 1000, 990))), NOW);
        assert_eq!(
            state.view().tiles.tick_avg_ms,
            None,
            "one scrape has no change"
        );
        state.scrape(
            URL,
            Ok(Scrape::parse(&tick_text(10.2, 1100, 1000))),
            NOW + SECOND_MS,
        );
        let tiles = state.view().tiles;
        let avg = tiles.tick_avg_ms.expect("avg");
        assert!((avg - 2.0).abs() < 1e-6, "0.2 s over 100 ticks: {avg}");
        assert_eq!(tiles.tick_p99_ms, Some(33.0));
    }

    #[test]
    fn rates_come_from_counter_deltas() {
        let counters = |packets: u64, bytes: u64, members: u64| {
            format!(
                "rep_gateway_datagrams_in_total {packets}\nrep_gateway_datagrams_out_total {packets}\n\
                 rep_gateway_bytes_in_total {bytes}\nrep_gateway_bytes_out_total {bytes}\n\
                 sim_bundle_members_total {members}\n"
            )
        };
        let mut state = LiveState::default();
        state.scrape(URL, Ok(Scrape::parse(&counters(100, 1000, 10))), NOW);
        state.scrape(
            URL,
            Ok(Scrape::parse(&counters(300, 5000, 70))),
            NOW + 2 * SECOND_MS,
        );
        let tiles = state.view().tiles;
        assert_eq!(tiles.datagrams_in_s, Some(100.0));
        assert_eq!(tiles.datagrams_out_s, Some(100.0));
        assert_eq!(tiles.bytes_in_s, Some(2000.0));
        assert_eq!(tiles.bytes_out_s, Some(2000.0));
        assert_eq!(tiles.members_s, Some(30.0));
    }

    #[test]
    fn a_down_metrics_port_gives_unknown_tiles() {
        let mut state = LiveState::default();
        let text = tick_text(10.0, 1000, 990);
        state.scrape(URL, Ok(Scrape::parse(&text)), NOW);
        state.scrape(OTHER_URL, Ok(Scrape::parse(&text)), NOW);
        state.scrape(URL, Err("connection refused".to_owned()), NOW + SECOND_MS);
        state.scrape(
            OTHER_URL,
            Ok(Scrape::parse(&tick_text(10.2, 1100, 1000))),
            NOW + SECOND_MS,
        );
        let view = state.view();
        let down = view.metrics.iter().find(|m| m.url == URL).expect("down");
        assert!(!down.up);
        assert_eq!((down.tick_avg_ms, down.tick_p99_ms), (None, None));
        let up = view
            .metrics
            .iter()
            .find(|m| m.url == OTHER_URL)
            .expect("up");
        assert!(up.up && up.tick_avg_ms.is_some());
        assert!(
            view.tiles.tick_avg_ms.is_some(),
            "the other page still counts"
        );
        let mut all_down = LiveState::default();
        all_down.scrape(URL, Err("refused".to_owned()), NOW);
        assert_eq!(all_down.view().tiles.tick_avg_ms, None);
        assert_eq!(all_down.view().tiles.bytes_in_s, None);
    }

    #[test]
    fn the_bot_tile_is_the_running_batch_total() {
        let mut state = LiveState::default();
        assert_eq!(state.view().tiles.bots, 0);
        state.bots(110);
        assert_eq!(state.view().tiles.bots, 110);
    }

    #[test]
    fn the_sim_of_a_point_comes_from_the_cell_table() {
        let mut state = LiveState::default();
        assert_eq!(state.sim_at(10.0, 10.0), None, "no table yet");
        let slot = |name: &str| SimSlot {
            id: sim(name),
            generation: 1,
        };
        state.table(
            CellTable {
                version: 1,
                cell_size_m: 1024.0,
                columns: 2,
                rows: 1,
                sims: vec![slot("sim-a"), slot("sim-b")],
                owners: vec![Some(0), Some(1)],
            },
            NOW,
        );
        assert_eq!(state.sim_at(10.0, 10.0), Some(sim("sim-a")));
        assert_eq!(state.sim_at(1500.0, 10.0), Some(sim("sim-b")));
        assert_eq!(state.sim_at(3000.0, 10.0), None, "off the grid");
        assert_eq!(state.sim_at(-1.0, 10.0), None);
    }

    #[test]
    fn mobs_and_player_positions_come_from_snapshots() {
        let mut state = LiveState::default();
        state.snapshot_part(part(NOW, 0, 1, &[5]));
        assert_eq!(state.sim_of_mob(100), Some(sim("sim-a")));
        assert_eq!(state.sim_of_mob(9), None);
        assert_eq!(state.player_position(5), Some([1.0, 2.0, 3.0]));
        assert_eq!(state.player_position(6), None);
    }

    #[test]
    fn a_table_change_is_an_event_and_is_shown() {
        let mut state = LiveState::default();
        let table = CellTable {
            version: 3,
            cell_size_m: 1024.0,
            columns: 2,
            rows: 1,
            sims: Vec::new(),
            owners: vec![None, None],
        };
        state.table(table.clone(), NOW);
        state.table(table, NOW);
        let view = state.view();
        assert_eq!(view.table.as_ref().map(|t| t.version), Some(3));
        let changes = texts(&view)
            .iter()
            .filter(|text| text.contains("cell table"))
            .count();
        assert_eq!(changes, 1, "the same version again is not an event");
    }

    #[test]
    fn the_event_log_keeps_the_newest() {
        let mut state = LiveState::default();
        for n in 0..EVENTS_KEPT + 5 {
            state.event(format!("event {n}"), NOW);
        }
        let view = state.view();
        assert_eq!(view.events.len(), EVENTS_KEPT);
        assert_eq!(
            view.events.last().map(|e| e.text.as_str()),
            Some(format!("event {}", EVENTS_KEPT + 4).as_str())
        );
    }
}
