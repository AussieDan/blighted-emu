// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replays the client input of retail tutorial captures through the sim's own movement of the
//! player ([`nw_sim_core::TrackedBody`], with the sim's reading of each input record:
//! [`sim::camera`]) over the level's ground, and compares the body with retail's own place.
//!
//! Retail's server runs the client's own character controller, so its own place is where the
//! client stands. The client corrects its own prediction toward the server's own place when the
//! two differ by more than its correction threshold, and snaps past the full-correction distance
//! (`scripts/patch-client.py`: 0.5 m and 2.0 m in the patched client). So the share of compared
//! places past those distances is how often our server pulls the client: the rubber banding.
//!
//! Open-loop: a server move of retail (a step of more than [`SERVER_MOVE_M`] between two own
//! places) puts the body at retail's new place, as the sim does with its own moves.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nw_capture::{CLIENT_TO_SERVER, Decoder, ledger_records};
use nw_content::client_build::load_client_build;
use nw_content::creature_animations::CreatureAnimations;
use nw_content::mob_prefabs::ASSET_ID_LEN;
use nw_content::player_attacks::{AttackMotion, Stance};
use nw_messages::{BodyCodec, ClientInputMsg, InputRecord, RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX};
use nw_sim_core::{
    AmongCharacters, CharacterBody, Ground, LevelGround, MountPaces, Position, TrackedBody,
};
use nw_world::WorldFile;
use nw_world::collision::{LevelCollision, collision_file_path};
use nw_world::collision_index::CollisionIndex;
use nw_world::roads::{RoadMap, road_file_path};
use nw_world::water::{WaterMap, water_file_path};
use nw_world::world_file::world_file_path;

use crate::ai_trace::{MobTrace, MountSample, PlayerSample, trace_ledger};
use crate::ftue_entities::read_ledger;

/// The client's correction threshold, m (patched client).
pub const CORRECTION_M: f32 = 0.5;
/// The client's full-correction distance, m (patched client).
pub const FULL_CORRECTION_M: f32 = 2.0;
/// A step of retail's own place longer than this between two states is a server move, m (the
/// fastest own movement, a mount's sprint, covers well under it in one state).
pub const SERVER_MOVE_M: f32 = 8.0;
/// The longest gap from an input to the next retail own place that the replay compares, ms (an
/// own state goes out every tick, 33 ms; a longer gap is a load or a cutscene).
pub const MAX_ANSWER_MS: u64 = 100;
/// Half of the input counter's range: a counter newer than the last read lies within it.
const COUNTER_HALF_RANGE: u16 = 0x8000;

/// One compared place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Compared {
    /// Ms from the first ledger record.
    pub time_ms: u64,
    /// Retail's own place.
    pub retail: Position,
    /// The replayed body.
    pub body: Position,
    /// The record's camera gave the body ([`nw_sim_core::Tracked::consistent`]).
    pub camera: bool,
}

impl Compared {
    /// The distance on the ground plane, m.
    pub fn horizontal_m(&self) -> f32 {
        self.body.horizontal_distance(self.retail)
    }

    /// The height difference, m (positive: the body is higher).
    pub fn height_m(&self) -> f32 {
        self.body.z - self.retail.z
    }
}

/// The summary of a replay.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplaySummary {
    /// Places compared.
    pub compared: usize,
    /// Server moves of retail.
    pub server_moves: usize,
    /// Horizontal error, m: median, 90th and 99th percentile.
    pub horizontal_m: [f32; 3],
    /// Height error (absolute), m: median, 90th and 99th percentile.
    pub height_m: [f32; 3],
    /// Share of places whose distance (3D) passes [`CORRECTION_M`].
    pub corrected_share: f32,
    /// Share of places whose distance (3D) passes [`FULL_CORRECTION_M`].
    pub snapped_share: f32,
    /// Share of places whose record's camera gave nothing.
    pub no_camera_share: f32,
}

/// The client input records of the ledger `bytes`, oldest first, each read once, with the time
/// of the message that carried it (ms from the first ledger record).
pub fn input_records(bytes: &[u8]) -> Vec<(u64, InputRecord, sim::camera::RecordView)> {
    let mut decoder = Decoder::new();
    let mut first_ms = None;
    let mut last_counter: Option<u16> = None;
    let mut out = Vec::new();
    for record in ledger_records(bytes) {
        let Ok(record) = record else {
            break;
        };
        let time_ms = record.time_ms - *first_ms.get_or_insert(record.time_ms);
        let output = decoder.record(&record);
        if record.direction != CLIENT_TO_SERVER {
            continue;
        }
        for delivered in &output.envelopes {
            if delivered.envelope.type_index() != Some(RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX) {
                continue;
            }
            let Ok(message) = ClientInputMsg::decode(&delivered.envelope.body) else {
                continue;
            };
            let views = sim::camera::record_views(&message.records);
            for (input, view) in message.records.iter().zip(views).rev() {
                let counter = input.u16_after_mask;
                let newer = last_counter.is_none_or(|last| {
                    let ahead = counter.wrapping_sub(last);
                    ahead != 0 && ahead < COUNTER_HALF_RANGE
                });
                if newer {
                    last_counter = Some(counter);
                    out.push((time_ms, input.clone(), view));
                }
            }
        }
    }
    out
}

/// A ride of the replay: retail's own mount states and the mount's speeds. The body gets on and
/// off as retail's `m_isMounted` says, and dashes as retail's dash flag says.
#[derive(Debug, Clone, Copy)]
pub struct Riding<'a> {
    /// Retail's own mount states, in time order.
    pub samples: &'a [MountSample],
    /// The speeds of the mount retail rode.
    pub paces: MountPaces,
}

/// Replays `records` over `ground` with the sea at `sea_level_m` against retail's own places
/// `retail` (in time order), on the mount of `riding` when retail rode; gives every compared
/// place.
pub fn replay(
    records: &[(u64, InputRecord, sim::camera::RecordView)],
    retail: &[PlayerSample],
    attacks: &[AttackStart],
    mobs: &MobBodies,
    riding: Option<Riding<'_>>,
    ground: &(impl Ground + ?Sized),
    sea_level_m: f32,
) -> (Vec<Compared>, usize) {
    let times = record_times(records);
    let mut next_attack = 0;
    let place = |sample: &PlayerSample| Position {
        x: sample.position[0],
        y: sample.position[1],
        z: sample.position[2],
    };
    let Some(first) = retail.first() else {
        return (Vec::new(), 0);
    };
    let mut body = TrackedBody::new(place(first));
    let mut next = 0;
    let mut last_retail = place(first);
    let mut compared = Vec::new();
    let mut server_moves = 0;
    let mut next_mount = 0;
    for ((time_ms, record, view), &record_ms) in records.iter().zip(&times) {
        if let Some(riding) = riding {
            while let Some(sample) = riding
                .samples
                .get(next_mount)
                .filter(|sample| sample.time_ms <= *time_ms)
            {
                match sample.mounted {
                    Some(true) if !body.is_mounted() => body.mount(riding.paces),
                    Some(false) => body.dismount(),
                    _ => {}
                }
                if let Some(dashing) = sample.dashing
                    && body.is_mounted()
                {
                    body.set_mount_dash(dashing);
                }
                next_mount += 1;
            }
        }
        let characters = mobs.at(*time_ms);
        let ground = &AmongCharacters {
            ground,
            characters: &characters,
        };
        while let Some(attack) = attacks
            .get(next_attack)
            .filter(|attack| attack.at_ms <= record_ms)
        {
            // An attack is a few seconds of milliseconds.
            #[allow(clippy::cast_precision_loss)]
            let since_ms = (record_ms - attack.at_ms) as f32;
            body.start_attack(
                Arc::clone(&attack.motion),
                attack.keys_from_ms,
                since_ms,
                ground,
                sea_level_m,
            );
            next_attack += 1;
        }
        // Retail's places up to this input: a server move puts the body there.
        while next < retail.len() && retail[next].time_ms <= *time_ms {
            let at = place(&retail[next]);
            if at.horizontal_distance(last_retail) > SERVER_MOVE_M {
                body.stand_at(at);
                server_moves += 1;
            }
            last_retail = at;
            next += 1;
        }
        let Some(input) = sim::camera::track_input(record, *view) else {
            continue;
        };
        let tracked = body.record(input, ground, sea_level_m);
        let answer = retail.get(next).filter(|sample| {
            sample.time_ms.saturating_sub(*time_ms) <= MAX_ANSWER_MS
                && place(sample).horizontal_distance(last_retail) <= SERVER_MOVE_M
        });
        if let Some(answer) = answer {
            compared.push(Compared {
                time_ms: answer.time_ms,
                retail: place(answer),
                body: tracked.position,
                camera: tracked.consistent,
            });
        }
    }
    (compared, server_moves)
}

/// The share of a correction one own place applies in the client
/// (`g_localPlayer_RequiredPhysicalCorrectionFraction` 0.5, `scripts/patch-client.py`).
pub const CORRECTION_SHARE: f32 = 0.5;

/// How far the client lets its own movement differ from the own place before it corrects itself
/// (`scripts/patch-client.py`, own-movement correction).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClientCorrection {
    /// `g_localPlayer_CorrectionThreshold`, m: no correction below it.
    pub threshold_m: f32,
    /// `g_localPlayer_ErrorDistanceForFullCorrection`, m: all of the way past it.
    pub full_m: f32,
}

/// The patched client until 2026-09-26.
pub const CORRECTION_UNTIL_2026_09_26: ClientCorrection = ClientCorrection {
    threshold_m: CORRECTION_M,
    full_m: FULL_CORRECTION_M,
};

/// The patched client (`CORRECTION_THRESHOLD_M` and `FULL_CORRECTION_M` of
/// `scripts/patch-client.py`).
pub const PATCHED_CORRECTION: ClientCorrection = ClientCorrection {
    threshold_m: 100_000.0,
    full_m: 100_000.0,
};

/// What the client went through in a closed-loop replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoopSummary {
    /// Own places the server sent.
    pub places_sent: usize,
    /// Places that pulled the client (past the correction threshold).
    pub pulls: usize,
    /// Of those, the pulls against the client's own motion: it went backward.
    pub backward_pulls: usize,
    /// Places that snapped it (past the full-correction distance).
    pub snaps: usize,
}

/// Replays `records` in a closed loop: the server sends its own place in every state (as the sim
/// does), the client walks retail's own path plus the corrections those places gave it under
/// `correction` (past the threshold it moves [`CORRECTION_SHARE`] of the way, past the full
/// distance all of it), its camera moves with it, and the server follows that camera. Each pull
/// moves the camera the server follows: the loop of live run 21 (2026-09-26), which the open-loop
/// [`replay`] cannot show.
pub fn closed_loop(
    records: &[(u64, InputRecord, sim::camera::RecordView)],
    retail: &[PlayerSample],
    ground: &(impl Ground + ?Sized),
    sea_level_m: f32,
    correction: ClientCorrection,
) -> LoopSummary {
    let place = |sample: &PlayerSample| Position {
        x: sample.position[0],
        y: sample.position[1],
        z: sample.position[2],
    };
    let mut summary = LoopSummary::default();
    let Some(first) = retail.first() else {
        return summary;
    };
    let mut body = TrackedBody::new(place(first));
    let mut next = 0;
    let mut last_retail = place(first);
    // The client's place less retail's: what the corrections moved it by.
    let mut offset = [0.0_f32; 3];
    for (time_ms, record, view) in records {
        let before = last_retail;
        while next < retail.len() && retail[next].time_ms <= *time_ms {
            let at = place(&retail[next]);
            if at.horizontal_distance(last_retail) > SERVER_MOVE_M {
                // A server move puts the client where the server puts it.
                body.stand_at(at);
                offset = [0.0; 3];
            }
            last_retail = at;
            next += 1;
        }
        let shifted = sim::camera::RecordView {
            camera: view.camera.map(|camera| Position {
                x: camera.x + offset[0],
                y: camera.y + offset[1],
                z: camera.z + offset[2],
            }),
            ..*view
        };
        let Some(input) = sim::camera::track_input(record, shifted) else {
            continue;
        };
        let tracked = body.record(input, ground, sea_level_m);
        summary.places_sent += 1;
        let gap = [
            tracked.position.x - (last_retail.x + offset[0]),
            tracked.position.y - (last_retail.y + offset[1]),
            tracked.position.z - (last_retail.z + offset[2]),
        ];
        let distance = gap.iter().map(|axis| axis * axis).sum::<f32>().sqrt();
        let share = if distance > correction.full_m {
            summary.snaps += 1;
            1.0
        } else if distance > correction.threshold_m {
            summary.pulls += 1;
            let motion = [last_retail.x - before.x, last_retail.y - before.y];
            if gap[0] * motion[0] + gap[1] * motion[1] < 0.0 {
                summary.backward_pulls += 1;
            }
            CORRECTION_SHARE
        } else {
            continue;
        };
        for (axis, step) in offset.iter_mut().zip(gap) {
            *axis += step * share;
        }
    }
    summary
}

/// What the client did in a keys-only window ([`key_windows`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Motion {
    /// No movement key and no action.
    Still,
    /// Movement keys, no action.
    Walking,
    /// An action (an attack, a dodge, an ability) started in the window.
    Acting,
    /// The body swam.
    Swimming,
}

/// One keys-only window: how the server's character moved by retail's keys alone, against how
/// retail's own place moved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyWindow {
    /// Its start, ms from the first ledger record.
    pub start_ms: u64,
    /// What the client did in it.
    pub motion: Motion,
    /// Retail's own place's motion in the window (end less start), m.
    pub retail: [f32; 3],
    /// The character's motion in the window, m.
    pub body: [f32; 3],
}

impl KeyWindow {
    /// The distance between the two motions on the ground plane, m.
    pub fn error_m(&self) -> f32 {
        (self.body[0] - self.retail[0]).hypot(self.body[1] - self.retail[1])
    }
}

/// The time of each of `records` (from [`input_records`]): a message's newest record at the
/// message's time, each older one [`sim::world::INPUT_RECORD_MS`] per counter step before it, as
/// the sim reads them.
pub fn record_times(records: &[(u64, InputRecord, sim::camera::RecordView)]) -> Vec<u64> {
    let mut times = vec![0; records.len()];
    let mut end = records.len();
    while end > 0 {
        let message_ms = records[end - 1].0;
        let newest = records[end - 1].1.u16_after_mask;
        let mut index = end;
        while index > 0 && records[index - 1].0 == message_ms {
            index -= 1;
            let behind = newest.wrapping_sub(records[index].1.u16_after_mask);
            times[index] = message_ms.saturating_sub(u64::from(
                behind.saturating_mul(sim::world::INPUT_RECORD_MS),
            ));
        }
        end = index;
    }
    times
}

/// An attack the server starts by the attack key, whose animation moves its player.
#[derive(Debug, Clone, PartialEq)]
pub struct AttackStart {
    /// When it starts, ms from the first ledger record.
    pub at_ms: u64,
    /// How it moves its player.
    pub motion: Arc<AttackMotion>,
    /// From this long after its start a movement key ends it (its next attack's start), ms.
    pub keys_from_ms: f32,
}

/// The attacks that a server reading the attack key of `records` at `times` with the weapon
/// stance `stance` starts ([`sim::world::AttackKeys`]), those whose animation moves the player.
pub fn attack_starts(
    records: &[(u64, InputRecord, sim::camera::RecordView)],
    times: &[u64],
    stance: &Stance,
) -> Vec<AttackStart> {
    let mut keys = sim::world::AttackKeys::NONE;
    records
        .iter()
        .zip(times)
        .filter_map(|((_, record, _), &time_ms)| {
            let attack = keys.record(Some(stance), record, time_ms, true)?;
            let basic = stance.schedule(attack.step)?.attack(attack.kind)?;
            // An attack's time is a few seconds of milliseconds.
            #[allow(clippy::cast_precision_loss)]
            let keys_from_ms = basic.next_attack_from_ms() as f32;
            Some(AttackStart {
                at_ms: attack.started_ms,
                motion: Arc::new(basic.motion.clone()?),
                keys_from_ms,
            })
        })
        .collect()
}

/// The mobs of a ledger as bodies on the ground ([`AmongCharacters`]): each one's places until
/// its death (its first `Death` fragment) or its last state, with its creature's capsule radius;
/// a mob whose creature has no radius is left out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MobBodies {
    mobs: Vec<(f32, MobPlaces)>,
}

/// One mob's places on the ground plane, by time (ms from the first ledger record).
type MobPlaces = Vec<(u64, [f32; 2])>;

/// The fragment names of a creature's deaths hold this.
const DEATH_FRAGMENT: &str = "Death";

impl MobBodies {
    /// The bodies of the mobs of `trace`, with the radii of `creatures`.
    pub fn of(trace: &[MobTrace], creatures: &CreatureAnimations) -> Self {
        let mobs = trace
            .iter()
            .filter_map(|mob| {
                let asset: [u8; ASSET_ID_LEN] = hex::decode(&mob.asset).ok()?.try_into().ok()?;
                let radius_m = creatures.get(&asset)?.collider_radius_m()?;
                let places = mob
                    .samples
                    .iter()
                    .take_while(|sample| {
                        !sample.full_body.as_ref().is_some_and(|entry| {
                            entry
                                .fragment_name
                                .as_deref()
                                .is_some_and(|name| name.contains(DEATH_FRAGMENT))
                        })
                    })
                    .filter_map(|sample| sample.position.map(|at| (sample.time_ms, [at[0], at[1]])))
                    .collect();
                Some((radius_m, places))
            })
            .collect();
        Self { mobs }
    }

    /// The bodies standing at `time_ms`: each mob's newest place not after it, while it lives.
    pub fn at(&self, time_ms: u64) -> Vec<CharacterBody> {
        self.mobs
            .iter()
            .filter_map(|(radius_m, places)| {
                let last = places.last()?;
                if time_ms > last.0 {
                    return None;
                }
                let index = places.partition_point(|(at_ms, _)| *at_ms <= time_ms);
                let (_, [x, y]) = places.get(index.checked_sub(1)?)?;
                Some(CharacterBody {
                    x: *x,
                    y: *y,
                    radius_m: *radius_m,
                })
            })
            .collect()
    }
}

/// The server's character against retail's own controller. Each window starts `window_ms` of
/// warm-up earlier at retail's own place, so the character is at speed when the window opens;
/// then the window compares the character's motion with retail's, both by retail's input records
/// alone (no camera), the attacks of `attacks` moving it by their animations. Retail's server ran
/// the client's own controller, so a controller that is the client's moves as retail's place
/// did. Windows with a server move are left out.
pub fn key_windows(
    records: &[(u64, InputRecord, sim::camera::RecordView)],
    retail: &[PlayerSample],
    attacks: &[AttackStart],
    mobs: &MobBodies,
    ground: &(impl Ground + ?Sized),
    sea_level_m: f32,
    window_ms: u64,
) -> Vec<KeyWindow> {
    let times = record_times(records);
    let place = |sample: &PlayerSample| Position {
        x: sample.position[0],
        y: sample.position[1],
        z: sample.position[2],
    };
    // Retail's place at `time_ms`: the newest sample not after it.
    let at = |time_ms: u64| {
        let index = retail.partition_point(|sample| sample.time_ms <= time_ms);
        index.checked_sub(1).map(|index| place(&retail[index]))
    };
    let motion_of = |from: Position, to: Position| [to.x - from.x, to.y - from.y, to.z - from.z];
    let mut windows = Vec::new();
    let (Some(first_ms), Some(last_ms)) = (
        records.first().map(|(time_ms, ..)| *time_ms),
        records.last().map(|(time_ms, ..)| *time_ms),
    ) else {
        return windows;
    };
    let mut warm_ms = first_ms;
    while warm_ms + 2 * window_ms <= last_ms {
        let (start_ms, end_ms) = (warm_ms + window_ms, warm_ms + 2 * window_ms);
        let span = (at(warm_ms), at(start_ms), at(end_ms));
        let moved = retail
            .iter()
            .filter(|sample| (warm_ms..=end_ms).contains(&sample.time_ms))
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| place(pair[0]).horizontal_distance(place(pair[1])) > SERVER_MOVE_M);
        let (Some(warm_at), Some(begin), Some(end)) = span else {
            warm_ms = start_ms;
            continue;
        };
        let first = records.partition_point(|(time_ms, ..)| *time_ms < warm_ms);
        let mut body = TrackedBody::new(warm_at);
        let mut body_begin = warm_at;
        let (mut keys, mut acted, mut swam) = (false, false, false);
        let mut next_attack = attacks.partition_point(|attack| attack.at_ms < warm_ms);
        for ((time_ms, record, view), &record_ms) in records[first..]
            .iter()
            .zip(&times[first..])
            .take_while(|((time_ms, ..), _)| *time_ms < end_ms)
        {
            let Some(mut input) = sim::camera::track_input(record, *view) else {
                continue;
            };
            input.view = None;
            let characters = mobs.at(*time_ms);
            let ground = AmongCharacters {
                ground,
                characters: &characters,
            };
            let ground = &ground;
            let mut attacked = false;
            while let Some(attack) = attacks
                .get(next_attack)
                .filter(|attack| attack.at_ms <= record_ms)
            {
                // An attack is a few seconds of milliseconds.
                #[allow(clippy::cast_precision_loss)]
                let since_ms = (record_ms - attack.at_ms) as f32;
                body.start_attack(
                    Arc::clone(&attack.motion),
                    attack.keys_from_ms,
                    since_ms,
                    ground,
                    sea_level_m,
                );
                attacked = true;
                next_attack += 1;
            }
            if *time_ms >= start_ms {
                let intent = input.character.intent;
                keys |= intent.forward != 0 || intent.right != 0;
                acted |= input.action_started || intent.dodge || attacked;
            } else {
                body_begin = body.position();
            }
            body.record(input, ground, sea_level_m);
            if *time_ms >= start_ms {
                swam |= body.is_swimming();
            } else {
                body_begin = body.position();
            }
        }
        if !moved {
            let motion = match (swam, acted, keys) {
                (true, ..) => Motion::Swimming,
                (false, true, _) => Motion::Acting,
                (false, false, true) => Motion::Walking,
                (false, false, false) => Motion::Still,
            };
            windows.push(KeyWindow {
                start_ms,
                motion,
                retail: motion_of(begin, end),
                body: motion_of(body_begin, body.position()),
            });
        }
        warm_ms = start_ms;
    }
    windows
}

/// The summary of `compared`.
pub fn summarize(compared: &[Compared], server_moves: usize) -> ReplaySummary {
    let percentiles = |mut values: Vec<f32>| {
        values.sort_by(f32::total_cmp);
        let at = |share: f32| {
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an index into tens of thousands of places"
            )]
            let index = ((values.len().saturating_sub(1)) as f32 * share) as usize;
            values.get(index).copied().unwrap_or(f32::NAN)
        };
        [at(0.5), at(0.9), at(0.99)]
    };
    let distance = |place: &Compared| place.horizontal_m().hypot(place.height_m());
    #[allow(clippy::cast_precision_loss, reason = "counts of tens of thousands")]
    let share = |limit: f32| {
        compared
            .iter()
            .filter(|place| distance(place) > limit)
            .count() as f32
            / compared.len().max(1) as f32
    };
    ReplaySummary {
        compared: compared.len(),
        server_moves,
        horizontal_m: percentiles(compared.iter().map(Compared::horizontal_m).collect()),
        height_m: percentiles(
            compared
                .iter()
                .map(|place| place.height_m().abs())
                .collect(),
        ),
        corrected_share: share(CORRECTION_M),
        snapped_share: share(FULL_CORRECTION_M),
        no_camera_share: compared.iter().filter(|place| !place.camera).count() as f32
            / compared.len().max(1) as f32,
    }
}

/// The ground of `level` of the content in `content` for client build config `build_config`:
/// the terrain, the level collision and the water, as the sim builds it; and the sea level.
pub fn level_ground(
    content: &Path,
    build_config: &Path,
    level: &str,
) -> (LevelGround<std::sync::Arc<WorldFile>>, f32) {
    use std::sync::Arc;
    let build = load_client_build(build_config).expect("client build");
    let world =
        Arc::new(WorldFile::load(&world_file_path(content, &build, level), &build).expect("world"));
    let collision = LevelCollision::load(&collision_file_path(content, &build, level), &build)
        .expect("run `just collision` first");
    let collision = Arc::new(CollisionIndex::new(LevelCollision {
        deferred: Vec::new(),
        ..collision
    }));
    let sea = world.level.ocean_level;
    let ground = LevelGround::new(Arc::clone(&world), collision);
    let ground = match WaterMap::load(&water_file_path(content, &build, level), &build) {
        Ok(water) => ground.with_water(Arc::new(water)),
        Err(_) => ground,
    };
    // The sim's ground holds the level's roads: a mount runs at its road speed on them.
    let ground = match RoadMap::load(&road_file_path(content, &build, level), &build) {
        Ok(roads) => ground.with_roads(Arc::new(roads)),
        Err(_) => ground,
    };
    (ground, sea)
}

/// The repository root.
pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[cfg(test)]
mod tests {
    use nw_content::player_attacks::{PlayerAttacks, stance_key};

    use super::*;

    const CLIENT_BUILD_CONFIG: &str = "config/client-build.toml";
    /// The weapon the tutorial gives (`1hSwordT2_FTUE`).
    const TUTORIAL_SWORD: &str = "1H_Melee,Sword";
    /// Until when each of [`LEDGERS`] holds the tutorial sword, ms from its first record
    /// (Observed: the own paperdoll's main hand, `run15/lockaim2/seqmine.py`: the blunderbuss from
    /// 448.2 s in the retail run, the great axe from 654.1 s in the Linux run).
    const SWORD_UNTIL_MS: [u64; 2] = [448_200, 654_100];
    const MEDIAN: f32 = 0.5;
    const NINETIETH: f32 = 0.9;

    /// One ledger as the replays read it: retail's own places, the input records, the attacks
    /// the server starts with the tutorial sword, and the mobs' bodies.
    struct Run {
        retail: Vec<PlayerSample>,
        records: Vec<(u64, InputRecord, sim::camera::RecordView)>,
        attacks: Vec<AttackStart>,
        mobs: MobBodies,
    }

    /// The run of `ledger`, whose player holds the tutorial sword until `sword_until_ms`.
    fn run_of(ledger: &str, sword_until_ms: u64) -> Run {
        let content = repo().join("local-data/content");
        let build = load_client_build(&repo().join(CLIENT_BUILD_CONFIG)).expect("client build");
        let table =
            PlayerAttacks::load(&content, &build).expect("run `content-tool player-attacks` first");
        let sword = table
            .stance(stance_key(TUTORIAL_SWORD))
            .expect("the sword's stance");
        let creatures = CreatureAnimations::load(&content, &build)
            .expect("run `content-tool creature-animations` first");
        let bytes = read_ledger(&repo().join(ledger)).expect("ledger");
        let trace = trace_ledger(&bytes, &[]);
        let records = input_records(&bytes);
        let attacks = attack_starts(&records, &record_times(&records), sword)
            .into_iter()
            .filter(|attack| attack.at_ms < sword_until_ms)
            .collect();
        Run {
            mobs: MobBodies::of(&trace.mobs, &creatures),
            retail: trace.player,
            records,
            attacks,
        }
    }

    /// The value at `share` of the sorted `values`; NaN without values.
    fn quantile(values: &[f32], share: f32) -> f32 {
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "an index into a few thousand values"
        )]
        let index = (values.len().saturating_sub(1) as f32 * share) as usize;
        values.get(index).copied().unwrap_or(f32::NAN)
    }
    const TUTORIAL_LEVEL: &str = "ftue_v2";
    /// The report's time windows, ms.
    const WINDOW_MS: u64 = 10_000;
    const MS_PER_SECOND: u64 = 1_000;
    const PERCENT: f32 = 100.0;
    /// The tutorial ship's deck lies above this height, m (the deck at 89.3 m, the sea at 20 m).
    const SHIP_DECK_ABOVE_M: f32 = 80.0;
    /// The tutorial level lies west of this x, m; the open world (the start beach at 9,600 m) east.
    const TUTORIAL_WEST_OF_M: f32 = 2_000.0;
    /// Whether a retail place lies in a part of the run.
    type InPart = fn(Position) -> bool;
    /// The parts of a tutorial run the report sums apart.
    const PARTS: [(&str, InPart); 3] = [
        ("ship", |at| {
            at.x < TUTORIAL_WEST_OF_M && at.z > SHIP_DECK_ABOVE_M
        }),
        ("sea and beach", |at| {
            at.x < TUTORIAL_WEST_OF_M && at.z <= SHIP_DECK_ABOVE_M
        }),
        ("open world", |at| at.x >= TUTORIAL_WEST_OF_M),
    ];
    /// The retail captures of the tutorial (a new character, ship to beach), each with the most
    /// share of its places the replay may pull the client in each of [`PARTS`] (the result of
    /// 2026-09-24 plus 0.02; `None`: the run has no such part). Lower one when a change brings
    /// the share down; a change that passes one is a step back.
    const LEDGERS: [(&str, [Option<f32>; 3]); 2] = [
        (
            "local-data/captures/own/newchar-retail-2026-09-18/ledger.bin",
            [Some(0.93), Some(0.47), Some(0.38)],
        ),
        (
            "local-data/captures/own/newchar-linux-2026-09-12/ledger.bin",
            [Some(0.41), Some(0.54), None],
        ),
    ];

    #[test]
    fn the_server_character_follows_retail_keys_like_the_client_controller() {
        /// One second of keys, after one second of warm-up: short enough that the camera is not
        /// needed, long enough to show speed, acceleration and turning.
        const WINDOW_MS: u64 = 1_000;
        let (ground, sea) = level_ground(
            &repo().join("local-data/content"),
            &repo().join(CLIENT_BUILD_CONFIG),
            TUTORIAL_LEVEL,
        );
        for ((ledger, _), sword_until_ms) in LEDGERS.into_iter().zip(SWORD_UNTIL_MS) {
            let Run {
                retail,
                records,
                attacks,
                mobs,
            } = run_of(ledger, sword_until_ms);
            let none = MobBodies::default();
            let keys_only = key_windows(&records, &retail, &[], &none, &ground, sea, WINDOW_MS);
            let windows = key_windows(&records, &retail, &attacks, &mobs, &ground, sea, WINDOW_MS);
            println!(
                "{ledger}: {} windows, {} attacks",
                windows.len(),
                attacks.len()
            );
            let acting = |set: &[KeyWindow]| {
                let starts: Vec<u64> = windows
                    .iter()
                    .filter(|window| window.motion == Motion::Acting)
                    .map(|window| window.start_ms)
                    .collect();
                let mut errors: Vec<f32> = set
                    .iter()
                    .filter(|window| starts.contains(&window.start_ms))
                    .map(KeyWindow::error_m)
                    .collect();
                errors.sort_by(f32::total_cmp);
                errors
            };
            for (name, errors) in [
                ("keys only", acting(&keys_only)),
                ("attacks", acting(&windows)),
            ] {
                println!(
                    "  the attack windows by {name}: median {:.2} m, 90% {:.2} m",
                    quantile(&errors, MEDIAN),
                    quantile(&errors, NINETIETH)
                );
            }
            for motion in [
                Motion::Still,
                Motion::Walking,
                Motion::Acting,
                Motion::Swimming,
            ] {
                let mut horizontal: Vec<f32> = windows
                    .iter()
                    .filter(|window| window.motion == motion)
                    .map(KeyWindow::error_m)
                    .collect();
                horizontal.sort_by(f32::total_cmp);
                println!(
                    "  {motion:?}: {} windows, error after 1 s median {:.2} m, 90% {:.2} m",
                    horizontal.len(),
                    quantile(&horizontal, MEDIAN),
                    quantile(&horizontal, NINETIETH)
                );
            }
            assert!(!windows.is_empty(), "{ledger}: no windows");
        }
    }

    #[test]
    fn the_patched_client_is_not_pulled_by_the_own_places_in_a_closed_loop() {
        let (ground, sea) = level_ground(
            &repo().join("local-data/content"),
            &repo().join(CLIENT_BUILD_CONFIG),
            TUTORIAL_LEVEL,
        );
        for (ledger, _) in LEDGERS {
            let bytes = read_ledger(&repo().join(ledger)).expect("ledger");
            let retail = trace_ledger(&bytes, &[]).player;
            let records = input_records(&bytes);
            let old = closed_loop(&records, &retail, &ground, sea, CORRECTION_UNTIL_2026_09_26);
            let now = closed_loop(&records, &retail, &ground, sea, PATCHED_CORRECTION);
            println!("{ledger}: 0.5 and 2.0 m {old:?}; patched {now:?}");
            // The check sees the loop: the old values pulled the client, also backward.
            assert!(
                old.pulls + old.snaps > 0 && old.backward_pulls > 0,
                "{old:?}"
            );
            assert_eq!(
                (now.pulls, now.snaps),
                (0, 0),
                "{ledger}: the client moved itself and was pulled"
            );
        }
    }

    #[test]
    #[ignore = "needs the local captures, the build 25810808 content and the open world level files"]
    fn the_mounted_own_movement_replays_against_retail() {
        use nw_content::ContentBundle;
        use nw_content::bundle::bundle_path;
        use nw_content::mounts::MountRules;
        use nw_content::progression::Progression;
        use nw_formats::objectstream::lowercase_crc32;
        /// Retail Hermit's Rally: the first race of the mount quest chain on Moonshadow.
        const LEDGER: &str = "local-data/captures/open-world/01-Get_Mounts-cd14fc4d0041e80e/\
            ef_tour_0-Hermit-s-Rally/ledger.bin";
        const OPEN_WORLD_LEVEL: &str = "newworld_vitaeeterna";
        const HORSE: &str = "Mount_Horse_1";
        /// Riding after the mount quest's 500 (its success event).
        const RIDING_EXPERIENCE: u32 = 500;
        /// The ride's errors with the rider's camera model: 1.11 m and 0.07 m (median).
        const RIDDEN_MEDIAN_M: f32 = 1.5;
        const RIDDEN_HEIGHT_MEDIAN_M: f32 = 0.2;
        let content = repo().join("local-data/content");
        let build = load_client_build(&repo().join(CLIENT_BUILD_CONFIG)).expect("client build");
        let bundle = ContentBundle::load(&bundle_path(&content, &build), &build).expect("bundle");
        let mounts = MountRules::from_tables(&bundle.tables);
        let progression = Progression::of_tables(&bundle.tables);
        let mount_type = mounts
            .mount(lowercase_crc32(HORSE))
            .and_then(|mount| mounts.mount_type(mount))
            .expect("the horse's type");
        let rank = progression
            .tradeskill(&mount_type.tradeskill)
            .map_or(0, |riding| riding.rank_of(RIDING_EXPERIENCE).0);
        let movement = mounts
            .movement(mount_type, rank)
            .expect("the horse's movement");
        let (ground, sea) = level_ground(
            &content,
            &repo().join(CLIENT_BUILD_CONFIG),
            OPEN_WORLD_LEVEL,
        );
        let bytes = read_ledger(&repo().join(LEDGER)).expect("ledger");
        let trace = trace_ledger(&bytes, &[]);
        let records = input_records(&bytes);
        let paces = MountPaces::of(movement);
        let riding = Riding {
            samples: &trace.player_mount,
            paces,
        };
        let (compared, moves) = replay(
            &records,
            &trace.player,
            &[],
            &MobBodies::default(),
            Some(riding),
            &ground,
            sea,
        );
        let mounted_at = |time_ms: u64| {
            trace
                .player_mount
                .iter()
                .take_while(|sample| sample.time_ms <= time_ms)
                .filter_map(|sample| sample.mounted)
                .last()
                .unwrap_or(false)
        };
        let (ridden, walked): (Vec<Compared>, Vec<Compared>) =
            compared.iter().partition(|place| mounted_at(place.time_ms));
        let ridden_summary = summarize(&ridden, moves);
        println!("rank {rank}, ridden: {ridden_summary:?}");
        println!("walked: {:?}", summarize(&walked, 0));
        assert!(!ridden.is_empty(), "the ride is compared");
        // The walking camera model held a rider 2.0 m up and 2.69 m behind (median).
        assert!(
            ridden_summary.horizontal_m[0] < RIDDEN_MEDIAN_M,
            "ridden median {} m",
            ridden_summary.horizontal_m[0]
        );
        assert!(
            ridden_summary.height_m[0] < RIDDEN_HEIGHT_MEDIAN_M,
            "ridden height median {} m",
            ridden_summary.height_m[0]
        );
    }

    #[test]
    fn the_tutorial_own_movement_replays_against_retail() {
        let (ground, sea) = level_ground(
            &repo().join("local-data/content"),
            &repo().join(CLIENT_BUILD_CONFIG),
            TUTORIAL_LEVEL,
        );
        for ((ledger, most_corrected), sword_until_ms) in LEDGERS.into_iter().zip(SWORD_UNTIL_MS) {
            let Run {
                retail, records, ..
            } = run_of(ledger, sword_until_ms);
            // The sim moves the body by the keys and the camera: not by the attacks' animations,
            // not among the mobs' bodies (the measure of both: the key windows).
            let (compared, moves) = replay(
                &records,
                &retail,
                &[],
                &MobBodies::default(),
                None,
                &ground,
                sea,
            );
            let summary = summarize(&compared, moves);
            println!("{ledger}: {summary:?}");
            for window in compared.chunk_by(|a, b| a.time_ms / WINDOW_MS == b.time_ms / WINDOW_MS) {
                let window_summary = summarize(window, 0);
                let first = window[0];
                println!(
                    "  {:>4} s at ({:.0}, {:.0}, {:.1}): h {:.2}/{:.2} m, z {:.2} m, corrected {:.0}%",
                    first.time_ms / MS_PER_SECOND,
                    first.retail.x,
                    first.retail.y,
                    first.retail.z,
                    window_summary.horizontal_m[0],
                    window_summary.horizontal_m[1],
                    window_summary.height_m[0],
                    window_summary.corrected_share * PERCENT
                );
            }
            for ((part, bounds), most) in PARTS.into_iter().zip(most_corrected) {
                let inside: Vec<Compared> = compared
                    .iter()
                    .filter(|place| bounds(place.retail))
                    .copied()
                    .collect();
                let part_summary = summarize(&inside, 0);
                println!("  {part}: {part_summary:?}");
                if let Some(most) = most {
                    assert!(
                        part_summary.corrected_share <= most,
                        "{ledger}, {part}: {} of the places pull the client (at most {most})",
                        part_summary.corrected_share
                    );
                }
            }
            let mut worst: Vec<&Compared> = compared.iter().collect();
            worst.sort_by(|a, b| {
                b.horizontal_m()
                    .hypot(b.height_m())
                    .total_cmp(&a.horizontal_m().hypot(a.height_m()))
            });
            for place in worst.iter().take(10) {
                println!("  worst {place:?}");
            }
            assert!(summary.compared > 0, "{ledger}: nothing compared");
        }
    }
}
