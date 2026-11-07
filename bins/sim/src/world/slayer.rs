// SPDX-License-Identifier: AGPL-3.0-or-later
//! The slayer state of a player (slayer note, 2026-09-18; movement study 2026-09-20; run-15 lock
//! and aim study 2026-09-22): the action it plays, which the client and the other players see in
//! the ALC children 13 to 24 and 48, and the time into that action's sequence (children 2 and 3).
//!
//! The client starts some actions itself and names each one in row 1 of its input
//! ([`SlayerInput`]); the server echoes it in the next own state, started in the state numbered
//! with that record's counter, then adds its follow-up for some of them. Every other action is
//! the server's (Observed, retail newchar capture 2026-09-18; the client waits for them): the
//! light attack at the release of a tap of the attack key, the charge after a hold, the dodge,
//! the jump, the block and the aim of the right-click, the weapon draw and the movement states.
//! Runs 13 and 15 sent none of the attack starts: their clients started no attack at all (37 and
//! 103 taps, every one shorter than the hold; retail's server started the first attack of each
//! chain at the release, 463 at record 0x6bd after the press at 0x6ba). The sequences come from
//! the player sequence table ([`PlayerSequences`]) by the weapon's stance.
//!
//! The sequence time is the sum of the input records' frame times since the record that started
//! the state, that record's own included ([`SequenceClock`] has the wire form).

use nw_content::player_sequences::{
    Direction, PlayerSequences, SlotsChange, StanceSequences, StatePlay,
};
use nw_messages::{
    RemoteUpper, SecondSlayerState, SequenceClock, SequenceTime, SlayerInput, SlayerState,
    SlotChange, UpperSlots,
};
use nw_sim_core::Intent;

/// How many own states carry one slayer change (Observed: retail sent a follow-up in the state
/// that started it and the next one, 15c1 and 15c2 at 206528 to 206577 ms).
const ECHO_STATES: u8 = 2;

/// The slayer states the server starts for the player's own movement with the weapon sheathed,
/// with the sequence each transition took most often in the retail newchar capture (the server's
/// slayer changes against the input keys of the records before and at them). The sequence of one
/// transition varies with what the capture does not show (the foot in front, the turn of the
/// camera); Unknown: the client's slayer script that picks it. A drawn weapon and a held
/// right-click take the stance's own ([`MoveSet::of`]).
mod movement {
    /// `slayerStateId` of a move that starts from standing still (`13=1a`).
    pub(super) const START_STATE: u64 = 0x1a;
    /// `slayerStateId` of the moves that follow and of a stop (`13=1b`).
    pub(super) const MOVE_STATE: u64 = 0x1b;
    /// `slayerStateId` of standing still (`13=1f`, the first own state's).
    pub(super) const IDLE_STATE: u64 = 0x1f;
    /// Standing still, a key forward or to the side: `aa2b`, 29 of the 57 forward starts and 11
    /// of the 21 side starts.
    pub(super) const START_SEQUENCE: u64 = 2794;
    /// Standing still, the back key: state `1b` with `9814`, 6 of the 16 back starts.
    pub(super) const BACK_START_SEQUENCE: u64 = 1304;
    /// Moving, the keys let go: `bd15`, 29 of the 96 stops of a forward move.
    pub(super) const STOP_SEQUENCE: u64 = 1405;
    /// Stopping, a key again: `b625`, 21 of the 51 forward presses during a stop.
    pub(super) const RESTART_SEQUENCE: u64 = 2422;
    /// Stopped: `b910`, 34 of the 106 idle states after a stop.
    pub(super) const IDLE_SEQUENCE: u64 = 1081;
    /// Input records from the stop to the idle state: the length of the stop sequence (9 of the
    /// 14 `bd15` stops that ended in an idle state did after 39 records).
    pub(super) const STOP_RECORDS: u16 = 39;
}

/// The slayer state of a player that a mob pulls (retail newchar 2026-09-12, Thorpe's first
/// grab pull at 456719 ms: `13=04 14=3163 15=8a37 16=00 17=3163` in the state numbered `3163`,
/// the pull's places in the next states, then `13=1f 15=b910`, standing still, at 457852 ms).
/// Without it the client kept moving by its own keys and dropped the pull (live 2026-09-26: the
/// body stood 7 m from its client after the first pull).
mod pull {
    /// `slayerStateId` of being pulled (`13=04`).
    pub(super) const STATE: u64 = 0x04;
    /// Its sequence: `8a37`.
    pub(super) const SEQUENCE: u64 = 3530;
    /// From the pull's state to the state that stands the player still again.
    pub(super) const LENGTH_MS: u64 = 1_133;
}

/// The kneel of a fast travel's channel (2026-11-07, the first slayer change in the 400 ms after
/// each retail 3713 and 3712 of `captures/own` and `captures/open-world`): the sequence alone,
/// started in the own state 40 to 80 ms after the call, with no state (`14=3fcc 15=05`), in 461
/// of the 3713 and 30 of 30 of the 3712. A rider first got off its mount: 5937 with reason 0
/// after all 23 such 3713, then the kneel with standing still (`13=1f 15=05`) in 11 of them; the
/// other 12 first played one of three dismount sequences (`13=0f` with `8721`, `882d` or `a90d`;
/// Unknown: the rule that picks one), so the kneel stands the rider still at once.
mod kneel {
    /// `slayerSequenceId` of the kneel: `05`.
    pub(super) const SEQUENCE: u64 = 5;
}

/// The slayer states the server sends when the player falls or swims (retail newchar 2026-09-18,
/// capture check 2026-09-24): `13=0d 15=9d3c` at the fall, `13=06 15=a021` once at the water
/// entry, then only the sequence (child 15, no 13 and no 14) while the player swims: `b621`
/// 1568 ms after the entry while it floats, `a52e` 31 to 43 ms after each forward key press,
/// `972e` and `a52e` in turn while the key stays down, and `b621` 34 ms after the key goes up.
/// None of the client's input records in the water named an action: the server sends them.
mod swim {
    /// `slayerStateId` of swimming (`13=06`).
    pub(super) const STATE: u64 = 0x06;
    /// `slayerStateId` of falling (`13=0d`).
    pub(super) const FALL_STATE: u64 = 0x0d;
    /// The water entry: `a021`.
    pub(super) const IDLE_SEQUENCE: u64 = 2144;
    /// Floating with no key: `b621`.
    pub(super) const FLOAT_SEQUENCE: u64 = 2166;
    /// The stroke at a key press, and every second one while the key stays down: `a52e`.
    pub(super) const STROKE_SEQUENCE: u64 = 2981;
    /// The stroke between two `a52e` while the key stays down: `972e`.
    pub(super) const NEXT_STROKE_SEQUENCE: u64 = 2967;
    /// Seconds from the entry to the float when no key is down (one entry: 815911 to 817479 ms).
    pub(super) const ENTRY_S: f32 = 1.568;
    /// Seconds of `a52e` before `972e` while the key stays down (median of 7, 0.669 to 1.502 s;
    /// Unknown: what makes it vary).
    pub(super) const STROKE_S: f32 = 1.098;
    /// Seconds of `972e` before `a52e` (median of 6, 1.230 to 1.283 s).
    pub(super) const NEXT_STROKE_S: f32 = 1.249;
    /// Falling: `9d3c` (best guess: retail started five different fall sequences and the rule
    /// that picks one is Unknown; this is one of them).
    pub(super) const FALL_SEQUENCE: u64 = 3869;
}

/// The second slayer slot while the inventory is open (retail newchar 2026-09-18, capture check
/// 2026-09-24, 5 of 5 opens): `16=23 18=962b` 30 to 40 ms after the client's open, `18=890a`
/// 1536 to 1568 ms later, `18=8b2e` 32 to 43 ms after its close, and `16=2d 18=00` 35 to 40 ms
/// after the second close the client sends 500 to 750 ms later. A miss of this slot rolls the
/// client's prediction back and closes the inventory (`FUN_1421ae2c0` / `FUN_14486f2a0`; live
/// 2026-09-24 without it: every open closed again 20 to 60 ms later). In 2 of the 5 opens the
/// first sequence was `a709` and `962b` followed 330 to 370 ms later with the idle state; Unknown:
/// the rule, so the open always plays `962b`.
mod loadout {
    /// `16=23`.
    pub(super) const OPEN_STATE: u64 = 0x23;
    /// `16=2d`.
    pub(super) const CLOSED_STATE: u64 = 0x2d;
    /// `962b`.
    pub(super) const OPEN_SEQUENCE: u64 = 2774;
    /// `890a`.
    pub(super) const LOOP_SEQUENCE: u64 = 649;
    /// `8b2e`.
    pub(super) const CLOSE_SEQUENCE: u64 = 2955;
    /// Time from the open to the loop sequence, ms (median of 5).
    pub(super) const LOOP_MS: u64 = 1_552;
}

/// The movement states and sequences of one moment: the sheathed ones, the stance's drawn ones,
/// or its block and aim walk while the right-click is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MoveSet {
    start: (u64, u64),
    back_start: (u64, u64),
    stop: (u64, u64),
    restart: (u64, u64),
    idle: (u64, u64),
}

impl MoveSet {
    /// The sheathed movement of the newchar capture.
    const SHEATHED: Self = Self {
        start: (movement::START_STATE, movement::START_SEQUENCE),
        back_start: (movement::MOVE_STATE, movement::BACK_START_SEQUENCE),
        stop: (movement::MOVE_STATE, movement::STOP_SEQUENCE),
        restart: (movement::MOVE_STATE, movement::RESTART_SEQUENCE),
        idle: (movement::IDLE_STATE, movement::IDLE_SEQUENCE),
    };

    /// The movement of `stance` (the weapon `drawn` or not) with the right-click held or not.
    /// A value the table does not have keeps the sheathed one (best guess: the table's drawn
    /// moves are the most common non-sheathed sequence of each transition per stance).
    fn of(stance: Option<&StanceSequences>, drawn: bool, alt: bool) -> Self {
        let mut set = Self::SHEATHED;
        let Some(stance) = stance.filter(|_| drawn) else {
            return set;
        };
        let pair = |play: StatePlay| (play.state, play.sequence);
        if let Some(moves) = stance.drawn_moves {
            set.start = moves.start.map_or(set.start, pair);
            set.back_start = moves.back_start.map_or(set.back_start, pair);
            set.stop.1 = moves.stop.unwrap_or(set.stop.1);
            set.restart.1 = moves.restart.unwrap_or(set.restart.1);
            set.idle.1 = moves.idle.unwrap_or(set.idle.1);
        }
        if alt && let Some(walk) = stance.alt_moves {
            if let Some(moving) = walk.moving.map(pair) {
                set.start = moving;
                set.back_start = moving;
                set.stop = moving;
                set.restart = moving;
            }
            set.idle = walk.idle.map_or(set.idle, pair);
        }
        set
    }
}

/// Where the player's own movement is, for its movement slayer states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Locomotion {
    /// Standing still (the first own state's idle state).
    #[default]
    Idle,
    /// Moving by its keys.
    Moving,
    /// Its keys let go: records left until it stands still.
    Stopping(u16),
}

impl Locomotion {
    /// The next movement after a record with `intent`, and the slayer state and sequence the
    /// server starts in that record, when it starts one.
    fn step(self, intent: Intent, set: MoveSet) -> (Self, Option<(u64, u64)>) {
        let moving = intent.forward != 0 || intent.right != 0;
        match (self, moving) {
            (Self::Idle, true) => {
                let start = if intent.forward < 0 {
                    set.back_start
                } else {
                    set.start
                };
                (Self::Moving, Some(start))
            }
            (Self::Moving, false) => (Self::Stopping(movement::STOP_RECORDS), Some(set.stop)),
            (Self::Stopping(_), true) => (Self::Moving, Some(set.restart)),
            (Self::Stopping(left), false) => match left.saturating_sub(1) {
                0 => (Self::Idle, Some(set.idle)),
                left => (Self::Stopping(left), None),
            },
            (Self::Idle, false) | (Self::Moving, true) => (self, None),
        }
    }

    /// The state the movement plays now in `set`: sent again when the set changes.
    const fn current(self, set: MoveSet) -> (u64, u64) {
        match self {
            Self::Idle => set.idle,
            Self::Moving => set.restart,
            Self::Stopping(_) => set.stop,
        }
    }
}

/// Where the player's swim is, for its swim sequences ([`swim`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum Swim {
    /// Out of the water.
    #[default]
    Out,
    /// Came into the water: seconds since, while no key is down.
    Entered(f32),
    /// Floating with no key down.
    Floating,
    /// Swimming by its keys: the stroke sequence it plays, and seconds into it.
    Stroking(u64, f32),
}

impl Swim {
    /// The next swim after a record in the water with `moving` keys and frame time `frame_s`,
    /// and the sequence the server sends in that record, when it sends one.
    fn step(self, moving: bool, frame_s: f32) -> (Self, Option<u64>) {
        match (self, moving) {
            (Self::Out | Self::Entered(_) | Self::Floating, true) => (
                Self::Stroking(swim::STROKE_SEQUENCE, 0.0),
                Some(swim::STROKE_SEQUENCE),
            ),
            (Self::Stroking(..), false) => (Self::Floating, Some(swim::FLOAT_SEQUENCE)),
            (Self::Entered(since_s), false) => {
                let since_s = since_s + frame_s;
                if since_s >= swim::ENTRY_S {
                    (Self::Floating, Some(swim::FLOAT_SEQUENCE))
                } else {
                    (Self::Entered(since_s), None)
                }
            }
            (Self::Stroking(sequence, into_s), true) => {
                let into_s = into_s + frame_s;
                let (length_s, next) = if sequence == swim::STROKE_SEQUENCE {
                    (swim::STROKE_S, swim::NEXT_STROKE_SEQUENCE)
                } else {
                    (swim::NEXT_STROKE_S, swim::STROKE_SEQUENCE)
                };
                if into_s >= length_s {
                    (Self::Stroking(next, 0.0), Some(next))
                } else {
                    (Self::Stroking(sequence, into_s), None)
                }
            }
            (Self::Out | Self::Floating, false) => (self, None),
        }
    }
}

/// The inventory's second slayer slot ([`loadout`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LoadoutPlay {
    /// No inventory slot.
    #[default]
    Closed,
    /// The open sequence, until [`loadout::LOOP_MS`].
    Opening {
        /// The ALC state that started it.
        started: u16,
        /// When the client opened it.
        opened_ms: u64,
        /// Own states left that carry it.
        left: u8,
    },
    /// The loop sequence while it stays open.
    Looping {
        /// The ALC state that started the open.
        started: u16,
        /// Own states left that carry it.
        left: u8,
    },
    /// The close sequence: own states left that carry it; then nothing until the client's
    /// second close.
    Closing {
        /// The ALC state that started the close.
        started: u16,
        /// Own states left that carry the close sequence.
        left: u8,
    },
    /// The clear to `16=2d` after the second close: own states left that carry it.
    Cleared {
        /// The ALC state that started the clear.
        started: u16,
        /// Own states left that carry the clear.
        left: u8,
    },
}

/// A slayer state that started in an input record and that no own state carried yet.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    /// A new sequence of the state that goes on (the swim strokes: child 15 alone).
    Sequence(u64),
    /// An action the client started.
    Client(SlayerInput),
    /// An attack the server started (the retail form of its starts and follow-ups: children 14,
    /// 15 and 17, no state).
    Attack(u64),
    /// A state the server started: its state and sequence (movement, dodge, jump).
    State(u64, u64),
}

/// What the kneel of a fast travel's channel starts from ([`kneel`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KneelFrom {
    /// The state the player is in goes on: the sequence alone.
    State,
    /// The player just got off its mount: the kneel stands it still.
    Dismount,
}

/// The weapon in the player's hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HeldWeapon {
    /// Its stance key ([`nw_content::player_attacks::stance_key`]).
    pub(super) stance: u32,
    /// Whether it is drawn.
    pub(super) drawn: bool,
    /// It has shots and none is loaded ([`super::gear::Gear::held_empty`]).
    pub(super) empty: bool,
}

/// What an attack key press changed in the upper slots ([`SlayerTrack::shot_slots`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shot {
    /// The reload: the press fires nothing.
    Reload(UpperSlots),
    /// The aimed shot.
    Aimed(UpperSlots),
}

/// The attack key in one input record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct AttackKey {
    /// It went down in this record.
    pub(super) pressed: bool,
    /// It came up in this record.
    pub(super) released: bool,
}

/// One input record for the slayer state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SlayerRecord {
    /// The record's counter.
    pub(super) counter: u16,
    /// Its frame time, s.
    pub(super) frame_s: f32,
    /// The action its client starts in it.
    pub(super) action: Option<SlayerInput>,
    /// Its movement keys, when the server moves the body by them; with them the dodge, jump and
    /// right-click keys.
    pub(super) intent: Option<Intent>,
    /// Its attack key.
    pub(super) attack: AttackKey,
    /// The weapon in the hand.
    pub(super) weapon: Option<HeldWeapon>,
    /// The body swims ([`nw_sim_core::TrackedBody::is_swimming`]).
    pub(super) swimming: bool,
    /// The body falls and is not in a jump ([`nw_sim_core::TrackedBody::is_falling`]).
    pub(super) falling: bool,
}

/// What one own state carries of the slayer state.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct SlayerDue {
    /// The slayer state, when it changed or echoes.
    pub(super) state: Option<SlayerState>,
    /// The time into its sequence.
    pub(super) time: Option<SequenceTime>,
    /// The slots past the first and the marker, when they changed or echo.
    pub(super) slots: Option<UpperSlots>,
}

/// One player's slayer state.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct SlayerTrack {
    /// A state that started in a record (its counter, and that record's frame time) and that no
    /// own state carried yet.
    pending: Option<(Pending, u16, f32)>,
    /// The follow-up that starts in the next own state.
    follow_up: Option<u64>,
    /// The follow-up that starts at the attack key's next release.
    release_follow_up: Option<u64>,
    /// The record of the attack key's press, while it is down and no action answered it.
    press: Option<u16>,
    /// The upper slots that changed in records no own state carried yet.
    upper: Option<UpperSlots>,
    /// The newest upper slot change, and how many more own states carry it.
    upper_current: Option<UpperSlots>,
    upper_repeats: u8,
    /// The newest upper slot change came after the newest slayer state started.
    upper_after_state: bool,
    /// The upper slots of the newest tick's own state, for the players who see this one; kept
    /// after the own state took them, until the next tick.
    remote: Option<RemoteUpper>,
    /// The previous record's weapon had shots and none loaded.
    was_empty: bool,
    /// The weapon was loaded again after it emptied and no reload played since.
    reload_due: bool,
    /// The previous record's keys: right-click, dodge, jump; and whether the weapon was drawn.
    alt_held: bool,
    dodge_held: bool,
    jump_held: bool,
    drawn: bool,
    /// The player's own movement.
    locomotion: Locomotion,
    /// The last record was a fall the server already started.
    in_air: bool,
    /// The player's swim.
    swim: Swim,
    /// The inventory's second slayer slot.
    loadout: LoadoutPlay,
    /// A mob pulls the player: the pull state starts in the next own state.
    pull_due: bool,
    /// When the pull ends and the player stands still again, while it is pulled.
    pull_ends_ms: Option<u64>,
    /// A fast travel's channel started: the kneel starts in the next own state, from the state
    /// that goes on or, after a dismount, standing still ([`kneel`]).
    kneel_due: Option<KneelFrom>,
    /// A fast travel's channel ended: the next own state stands the player still, unless a
    /// state of its own records starts there ([`kneel`]).
    stand_due: bool,
    /// The newest slayer state, and how many more own states carry it.
    current: Option<SlayerState>,
    repeats: u8,
    /// Seconds into the newest state's sequence.
    sequence_s: f32,
    /// The frame time of the newest record.
    newest_frame_s: f32,
    /// The sequence time the client has; `None` before its first own state.
    clock: Option<SequenceClock>,
    /// What this tick's own state carries, until that state takes it.
    due: SlayerDue,
}

impl SlayerTrack {
    /// The player's client took its first own state ([`nw_messages::AlcState::spawn`]): its
    /// slots and sequence time start there, standing still.
    pub(super) fn spawned(&mut self) {
        *self = Self {
            sequence_s: SequenceClock::spawn_seconds(),
            clock: Some(SequenceClock::after_spawn()),
            ..Self::default()
        };
    }

    /// Reads one input record not read before (oldest first), with the sequences of `table`:
    /// its frame time goes on the sequence; an action its client starts goes out with the next
    /// own state, else a state the server starts for its keys (dodge, jump, attack, movement);
    /// the right-click and the draw change the upper slots.
    pub(super) fn record(&mut self, record: SlayerRecord, table: &PlayerSequences) {
        self.newest_frame_s = record.frame_s;
        self.sequence_s += record.frame_s;
        let stance = record.weapon.and_then(|weapon| table.stance(weapon.stance));
        let drawn = record.weapon.is_some_and(|weapon| weapon.drawn);
        let upper = self.upper_slots(&record, stance, drawn);
        let alt = drawn && record.intent.is_some_and(|intent| intent.block);
        let set = MoveSet::of(stance, drawn, alt);
        let entered_air = record.falling && !record.swimming && !self.in_air;
        let entered_water = record.swimming && self.swim == Swim::Out;
        self.in_air = record.falling && !record.swimming;
        let mut stroke = None;
        if entered_water {
            self.locomotion = Locomotion::Idle;
            self.swim = Swim::Entered(0.0);
        } else if !record.swimming {
            self.swim = Swim::Out;
        } else if let Some(intent) = record.intent {
            let moving = intent.forward != 0 || intent.right != 0;
            let (next, sequence) = self.swim.step(moving, record.frame_s);
            self.swim = next;
            stroke = sequence;
        }
        let mut movement = if entered_air {
            Some((swim::FALL_STATE, swim::FALL_SEQUENCE))
        } else if entered_water {
            Some((swim::STATE, swim::IDLE_SEQUENCE))
        } else if record.swimming {
            None
        } else {
            record.intent.and_then(|intent| {
                let (next, started) = self.locomotion.step(intent, set);
                self.locomotion = next;
                started
            })
        };
        if upper.is_some() && movement.is_none() && !self.keeps_attack() {
            // The walk changes with the block or aim: retail sent the new walk sequence in the
            // state of the right-click (`15=af10` with the aim). A draw that arrives after a
            // tap has already started the light must not replace that attack with a walk.
            movement = Some(self.locomotion.current(set));
        }
        if let Some(upper) = upper {
            self.upper = Some(merge(self.upper.unwrap_or_default(), upper));
        }
        let shot = self.shot_slots(&record, stance, drawn);
        if let Some(Shot::Reload(slots) | Shot::Aimed(slots)) = shot {
            self.upper = Some(merge(self.upper.unwrap_or_default(), slots));
        }
        let server = self.server_start(&record, stance, table);
        if matches!(shot, Some(Shot::Reload(_))) {
            // The reload's press is no tap: its release starts no attack.
            self.press = None;
        }
        let started = record
            .action
            .map(Pending::Client)
            .or(server)
            .or_else(|| movement.map(|(state, sequence)| Pending::State(state, sequence)))
            .or_else(|| stroke.map(Pending::Sequence));
        if let Some(input) = record.action {
            self.press = None;
            match table.follow_up(input.sequence) {
                Some(follow_up) if follow_up.at_release && !record.attack.released => {
                    self.release_follow_up = Some(follow_up.server);
                }
                Some(follow_up) => self.queue_follow_up(follow_up.server),
                None => self.release_follow_up = None,
            }
        }
        if let Some(started) = started {
            self.pending = Some((started, record.counter, record.frame_s));
            if !matches!(started, Pending::Sequence(_)) {
                // A new sequence of a state that goes on keeps that state's time.
                self.sequence_s = record.frame_s;
            }
            if !matches!(started, Pending::Client(_)) {
                self.follow_up = None;
            }
        }
    }

    /// Queues `sequence` to start one own state after the client's action (the retail
    /// follow-ups that came without a key, 0 to 132 ms after it).
    fn queue_follow_up(&mut self, sequence: u64) {
        self.follow_up = Some(sequence);
        self.release_follow_up = None;
    }

    /// The state the server starts for the keys of `record` with the sequences of `stance`:
    /// a dodge or a jump at its key's press (state 07 and 0e, by direction); at a release of
    /// the attack key the follow-up of the client's last action, else the light attack of a tap
    /// that no action answered; the charge when the key stayed down
    /// [`PlayerSequences::hold_records`] records. No attack starts while the right-click is held
    /// (Observed: the aimed shots of the retail newchar capture were the client's; the server
    /// sent no start at their releases).
    fn server_start(
        &mut self,
        record: &SlayerRecord,
        stance: Option<&StanceSequences>,
        table: &PlayerSequences,
    ) -> Option<Pending> {
        let intent = record.intent?;
        let dodge = intent.dodge && !self.dodge_held;
        let jump = intent.jump && !self.jump_held;
        self.dodge_held = intent.dodge;
        self.jump_held = intent.jump;
        let direction = Direction::of_keys(
            intent.forward > 0,
            intent.forward < 0,
            intent.right < 0,
            intent.right > 0,
        );
        let state = |play: StatePlay| Pending::State(play.state, play.sequence);
        let keyed = if dodge {
            stance
                .and_then(|stance| stance.dodge?.of(direction))
                .map(state)
        } else if jump {
            stance
                .and_then(|stance| stance.jump?.of(direction))
                .map(state)
        } else {
            None
        };
        if record.attack.pressed {
            self.press = Some(record.counter);
        }
        let aiming = intent.block;
        let attack = if record.attack.released {
            let tapped = self.press.take().filter(|_| !aiming);
            self.release_follow_up.take().or_else(|| {
                tapped?;
                let moving = intent.forward != 0 || intent.right != 0;
                stance?.light_of(moving)
            })
        } else if let Some(press) = self.press
            && record.counter.wrapping_sub(press) == table.hold_records
            && !aiming
        {
            self.press = None;
            stance.and_then(|stance| stance.charge)
        } else {
            None
        };
        keyed.or_else(|| attack.map(Pending::Attack))
    }

    /// The upper slot change of `record`: the block or aim at the right-click's press and its
    /// end at the release, with the weapon drawn; the draw's second slot when the weapon came
    /// to the hand. A slot whose state or sequence goes out starts in the record (retail: the
    /// aim's `20` is the right-click record), except the second slot without a state (retail's
    /// block sends child 18 alone).
    fn upper_slots(
        &mut self,
        record: &SlayerRecord,
        stance: Option<&StanceSequences>,
        drawn: bool,
    ) -> Option<UpperSlots> {
        let alt = drawn && record.intent.is_some_and(|intent| intent.block);
        let was_alt = self.alt_held;
        let was_drawn = self.drawn;
        if record.intent.is_some() {
            self.alt_held = alt;
        }
        self.drawn = drawn;
        let stance = stance?;
        let change = (if alt && !was_alt {
            stance.alt_start
        } else if !alt && was_alt && record.intent.is_some() {
            stance.alt_end
        } else if drawn && !was_drawn {
            stance
                .draw
                .map(|draw| nw_content::player_sequences::SlotsChange {
                    second_state: Some(draw.state),
                    second_sequence: Some(draw.sequence),
                    ..Default::default()
                })
        } else {
            None
        })?;
        slots_of(change, record.counter)
    }

    /// The upper slot change of an attack key press with the weapon drawn ([`Shot`]): the
    /// stance's reload when the weapon was loaded again after it emptied and no reload played
    /// since, else, with the right-click held and a shot loaded, its aimed shot. Retail
    /// blunderbuss (newchar 2026-09-18): the aimed shot `18=5e 48=29` 27 ms after the press; the
    /// presses while empty changed nothing; the first press 2.1 s after the shot sent the reload
    /// `16=24 17=557a 18=8142`, and the next press shot again.
    fn shot_slots(
        &mut self,
        record: &SlayerRecord,
        stance: Option<&StanceSequences>,
        drawn: bool,
    ) -> Option<Shot> {
        let empty = record.weapon.is_some_and(|weapon| weapon.empty);
        if self.was_empty && !empty {
            self.reload_due = true;
        }
        self.was_empty = empty;
        if !drawn || !record.attack.pressed {
            return None;
        }
        let stance = stance?;
        if std::mem::take(&mut self.reload_due) {
            return slots_of(stance.reload?, record.counter).map(Shot::Reload);
        }
        let aiming = record.intent.is_some_and(|intent| intent.block);
        if !aiming || empty {
            return None;
        }
        slots_of(stance.alt_fire?, record.counter).map(Shot::Aimed)
    }

    /// A mob pulls the player at `now_ms`: the next own state starts the pull state, and the
    /// player stands still again [`pull::LENGTH_MS`] later.
    pub(super) fn pulled(&mut self, now_ms: u64) {
        self.pull_due = true;
        self.pull_ends_ms = Some(now_ms.saturating_add(pull::LENGTH_MS));
    }

    /// A fast travel's channel started from `from`: the next own state starts the kneel, and
    /// the player stands still under it ([`kneel`]).
    pub(super) fn kneel(&mut self, from: KneelFrom) {
        self.kneel_due = Some(from);
        self.locomotion = Locomotion::Idle;
    }

    /// A fast travel's channel ended (arrived or cancelled): the next own state stands the
    /// player still with the second slot cleared, unless a state of its records starts there
    /// ([`kneel`]).
    pub(super) fn stand(&mut self) {
        self.kneel_due = None;
        self.stand_due = true;
        self.locomotion = Locomotion::Idle;
    }

    /// The client opened its inventory in record `started` at `now_ms`: the second slot plays the
    /// open sequence ([`loadout`]).
    pub(super) fn open_loadout(&mut self, started: u16, now_ms: u64) {
        self.loadout = LoadoutPlay::Opening {
            started,
            opened_ms: now_ms,
            left: ECHO_STATES,
        };
    }

    /// The client closed its inventory in record `started`: the first close plays the close
    /// sequence, the second clears the slot ([`loadout`]); a close with no open does nothing.
    pub(super) fn close_loadout(&mut self, started: u16) {
        self.loadout = match self.loadout {
            LoadoutPlay::Opening { .. } | LoadoutPlay::Looping { .. } => LoadoutPlay::Closing {
                started,
                left: ECHO_STATES,
            },
            LoadoutPlay::Closing { .. } => LoadoutPlay::Cleared {
                started,
                left: ECHO_STATES,
            },
            play @ (LoadoutPlay::Closed | LoadoutPlay::Cleared { .. }) => play,
        };
    }

    /// Moves the inventory slot on at `state_ms`: the loop after [`loadout::LOOP_MS`], and the
    /// end of the clear.
    fn step_loadout(&mut self, state_ms: u64) {
        match self.loadout {
            LoadoutPlay::Opening {
                started, opened_ms, ..
            } if state_ms.saturating_sub(opened_ms) >= loadout::LOOP_MS => {
                self.loadout = LoadoutPlay::Looping {
                    started,
                    left: ECHO_STATES,
                };
            }
            LoadoutPlay::Cleared { left: 0, .. } => self.loadout = LoadoutPlay::Closed,
            _ => {}
        }
    }

    /// The inventory's second slot for this own state, while a change of it echoes.
    fn loadout_slots(&self) -> Option<UpperSlots> {
        let (state, started, sequence) = match self.loadout {
            LoadoutPlay::Closed
            | LoadoutPlay::Opening { left: 0, .. }
            | LoadoutPlay::Looping { left: 0, .. }
            | LoadoutPlay::Closing { left: 0, .. }
            | LoadoutPlay::Cleared { left: 0, .. } => return None,
            LoadoutPlay::Opening { started, .. } => {
                (Some(loadout::OPEN_STATE), started, loadout::OPEN_SEQUENCE)
            }
            LoadoutPlay::Looping { started, .. } => {
                (Some(loadout::OPEN_STATE), started, loadout::LOOP_SEQUENCE)
            }
            LoadoutPlay::Closing { started, .. } => (None, started, loadout::CLOSE_SEQUENCE),
            LoadoutPlay::Cleared { started, .. } => (Some(loadout::CLOSED_STATE), started, 0),
        };
        Some(UpperSlots {
            second: SlotChange {
                state,
                started: Some(started),
                sequence: Some(sequence),
            },
            ..UpperSlots::default()
        })
    }

    /// One own state carried the inventory slot.
    fn consume_loadout_echo(&mut self) {
        if let LoadoutPlay::Opening { left, .. }
        | LoadoutPlay::Looping { left, .. }
        | LoadoutPlay::Closing { left, .. }
        | LoadoutPlay::Cleared { left, .. } = &mut self.loadout
        {
            *left = left.saturating_sub(1);
        }
    }

    /// Moves on to the tick whose own state is numbered `number` at state time `state_ms`: its
    /// slayer state is a newly started one, else the server's follow-up, else the last change once
    /// more ([`ECHO_STATES`]); its sequence time goes with every own state. The players who see
    /// this one get the newest state at once ([`SlayerTrack::current`]).
    pub(super) fn advance(&mut self, number: u16, state_ms: u64) {
        self.step_loadout(state_ms);
        if self.pull_ends_ms.is_some_and(|ends_ms| state_ms >= ends_ms) {
            self.pull_ends_ms = None;
            self.locomotion = Locomotion::Idle;
            self.pending = Some((
                Pending::State(movement::IDLE_STATE, movement::IDLE_SEQUENCE),
                number,
                self.newest_frame_s,
            ));
        }
        let (state, started) = self.next(number);
        let sequence_s = self.sequence_s;
        let time = self
            .clock
            .as_mut()
            .map(|clock| clock.time(sequence_s, started, state_ms));
        let slots = match (self.next_upper(), self.loadout_slots()) {
            (Some(upper), Some(loadout)) => Some(merge(upper, loadout)),
            (Some(slots), None) | (None, Some(slots)) => Some(slots),
            (None, None) => None,
        };
        self.consume_loadout_echo();
        self.remote = slots.map(|slots| RemoteUpper {
            slots,
            after_state: self.upper_after_state,
        });
        self.due = SlayerDue { state, time, slots };
    }

    /// What this tick's own state carries, once.
    pub(super) fn take_due(&mut self) -> SlayerDue {
        std::mem::take(&mut self.due)
    }

    /// An attack the server started is still this state's action: a pending start, or the echo
    /// of one (`state` empty and the second slot started with it, retail's attack form).
    fn keeps_attack(&self) -> bool {
        matches!(self.pending, Some((Pending::Attack(_), _, _)))
            || (self.repeats > 0
                && self
                    .current
                    .is_some_and(|state| state.state.is_none() && state.second.is_some()))
    }

    /// The upper slot change of the next own state: a new one, else the last once more.
    fn next_upper(&mut self) -> Option<UpperSlots> {
        if let Some(upper) = self.upper.take() {
            self.upper_current = Some(upper);
            self.upper_repeats = ECHO_STATES - 1;
            self.upper_after_state = true;
            return Some(upper);
        }
        if self.upper_repeats == 0 {
            return None;
        }
        self.upper_repeats -= 1;
        self.upper_current
    }

    /// The slayer state of the own state numbered `number`, and the frame time of the record that
    /// started it when it starts there.
    fn next(&mut self, number: u16) -> (Option<SlayerState>, Option<f32>) {
        let stand = std::mem::take(&mut self.stand_due);
        if std::mem::take(&mut self.pull_due) {
            self.sequence_s = self.newest_frame_s;
            return (self.start(pulled_state(number)), Some(self.newest_frame_s));
        }
        if let Some(from) = self.kneel_due.take() {
            self.sequence_s = self.newest_frame_s;
            return (
                self.start(kneel_state(number, from)),
                Some(self.newest_frame_s),
            );
        }
        if let Some((pending, counter, frame_s)) = self.pending.take() {
            let state = match pending {
                Pending::Client(input) => SlayerState::of_input(input, counter),
                Pending::Attack(sequence) => attack_start(counter, sequence),
                Pending::State(state, sequence) => SlayerState {
                    state: Some(state),
                    started: Some(counter),
                    sequence,
                    second: None,
                },
                Pending::Sequence(sequence) => {
                    let state = SlayerState {
                        state: None,
                        started: None,
                        sequence,
                        second: None,
                    };
                    return (self.start(state), None);
                }
            };
            return (self.start(state), Some(frame_s));
        }
        if stand {
            self.sequence_s = self.newest_frame_s;
            return (self.start(stand_state(number)), Some(self.newest_frame_s));
        }
        if let Some(sequence) = self.follow_up.take() {
            self.sequence_s = self.newest_frame_s;
            return (
                self.start(attack_start(number, sequence)),
                Some(self.newest_frame_s),
            );
        }
        if self.repeats == 0 {
            return (None, None);
        }
        self.repeats -= 1;
        (self.current, None)
    }

    /// The newest slayer state, for the players who see this one.
    pub(super) const fn current(&self) -> Option<SlayerState> {
        self.current
    }

    /// The upper slots of this tick's own state, for the players who see this one: retail's
    /// remote states echoed them as the own ones (arena capture 2026-09-13, a remote aim
    /// `16=24 18=9837 19=2e 21=9606 48=28` in 3 to 4 states, then without them).
    pub(super) const fn remote_upper(&self) -> Option<RemoteUpper> {
        self.remote
    }

    fn start(&mut self, state: SlayerState) -> Option<SlayerState> {
        self.upper_after_state = false;
        self.current = Some(state);
        self.repeats = ECHO_STATES - 1;
        self.current
    }
}

/// An attack the server starts in the state numbered `started`: retail's form of its starts and
/// follow-ups (`14=06bd 15=8f07 17=06bd`: no state, the second slot started with it).
const fn attack_start(started: u16, sequence: u64) -> SlayerState {
    SlayerState {
        state: None,
        started: Some(started),
        sequence,
        second: Some(SecondSlayerState {
            started,
            playing: None,
            stopped: false,
        }),
    }
}

/// The pull state in the own state numbered `started`: the second slot's action stops with it.
const fn pulled_state(started: u16) -> SlayerState {
    SlayerState {
        state: Some(pull::STATE),
        started: Some(started),
        sequence: pull::SEQUENCE,
        second: Some(SecondSlayerState {
            started,
            playing: None,
            stopped: true,
        }),
    }
}

/// The kneel of a fast travel's channel in the own state numbered `started`, from `from`
/// ([`kneel`]).
const fn kneel_state(started: u16, from: KneelFrom) -> SlayerState {
    SlayerState {
        state: match from {
            KneelFrom::State => None,
            KneelFrom::Dismount => Some(movement::IDLE_STATE),
        },
        started: Some(started),
        sequence: kneel::SEQUENCE,
        second: None,
    }
}

/// Standing still with the second slot cleared (`16=2d 18=00`), in the own state numbered
/// `started`: the state after a fast travel's channel. The first slayer change after 65 of the
/// retail 3696 (fast travel completed) of the kneel's captures was `13=1f 15=b910 16=2d 18=00`.
/// Without it the client kept the channel's pose after the loading screen (live 2026-11-07: the
/// sword and shield showed in the hands while the server held them sheathed).
const fn stand_state(started: u16) -> SlayerState {
    SlayerState {
        state: Some(movement::IDLE_STATE),
        started: Some(started),
        sequence: movement::IDLE_SEQUENCE,
        second: Some(SecondSlayerState {
            started,
            playing: Some((loadout::CLOSED_STATE, 0)),
            stopped: false,
        }),
    }
}

/// The upper slots that `change` sends, started in record `counter`: a slot whose state or
/// sequence goes out starts there, except the second slot without a state (retail's block sends
/// child 18 alone). `None` when it sends nothing.
fn slots_of(change: SlotsChange, counter: u16) -> Option<UpperSlots> {
    let slot = |state: Option<u64>, sequence: Option<u64>, starts: bool| SlotChange {
        state,
        started: (starts && (state.is_some() || sequence.is_some())).then_some(counter),
        sequence,
    };
    let slots = UpperSlots {
        second: slot(
            change.second_state,
            change.second_sequence,
            change.second_state.is_some(),
        ),
        third: slot(change.third_state, change.third_sequence, true),
        fourth: slot(change.fourth_state, change.fourth_sequence, true),
        marker: change.marker,
    };
    (slots != UpperSlots::default()).then_some(slots)
}

/// The upper slots of `older` with the children `newer` sends in their place.
fn merge(older: UpperSlots, newer: UpperSlots) -> UpperSlots {
    let slot = |older: SlotChange, newer: SlotChange| SlotChange {
        state: newer.state.or(older.state),
        started: newer.started.or(older.started),
        sequence: newer.sequence.or(older.sequence),
    };
    UpperSlots {
        second: slot(older.second, newer.second),
        third: slot(older.third, newer.third),
        fourth: slot(older.fourth, newer.fourth),
        marker: newer.marker.or(older.marker),
    }
}

#[cfg(test)]
mod tests {
    use nw_content::player_attacks::stance_key;
    use nw_content::player_sequences::{AltMoves, Directional, DrawnMoves, FollowUp, SlotsChange};

    use super::*;

    /// The 30 Hz frame time.
    const FRAME_S: f32 = 1.0 / 30.0;
    const TICK_MS: u64 = 33;
    /// The greatsword's tags and the mined table's values for it.
    const GREATSWORD: &str = "2H_Melee,GreatSword";
    const LIGHT: u64 = 4510;
    const CHARGE: u64 = 57;
    const HOLD_RECORDS: u16 = 19;
    const BLOCK_SEQUENCE: u64 = 2351;
    const BLOCK_WALK: StatePlay = StatePlay {
        state: 27,
        sequence: 1251,
    };
    const DRAWN_START: StatePlay = StatePlay {
        state: 26,
        sequence: 2205,
    };
    const DODGE_BACK: StatePlay = StatePlay {
        state: 7,
        sequence: 2100,
    };
    /// The blunderbuss's tags and its aim (retail newchar `19=2e 21=9706 48=28`).
    const BLUNDERBUSS: &str = "2H_Ranged,Blunderbuss";
    const AIM_STATE: u64 = 0x2e;
    const AIM_SEQUENCE: u64 = 407;
    const AIM_MARKER: u64 = 0x28;
    /// The blunderbuss's aimed shot (`18=5e 48=29`) and reload (`16=24 18=8142`) of the table.
    const SHOT_SEQUENCE: u64 = 94;
    const SHOT_MARKER: u64 = 41;
    const RELOAD_STATE: u64 = 36;
    const RELOAD_SEQUENCE: u64 = 4225;

    /// The retail follow-up: record 5568 started sequence 5055; the server followed with 4603.
    fn table() -> PlayerSequences {
        PlayerSequences::of_parts(
            HOLD_RECORDS,
            vec![
                StanceSequences {
                    tags: GREATSWORD.to_owned(),
                    light: Some(LIGHT),
                    charge: Some(CHARGE),
                    alt_start: Some(SlotsChange {
                        second_sequence: Some(BLOCK_SEQUENCE),
                        ..SlotsChange::default()
                    }),
                    alt_moves: Some(AltMoves {
                        moving: Some(BLOCK_WALK),
                        idle: None,
                    }),
                    drawn_moves: Some(DrawnMoves {
                        start: Some(DRAWN_START),
                        ..DrawnMoves::default()
                    }),
                    dodge: Some(Directional {
                        back: Some(DODGE_BACK),
                        ..Directional::default()
                    }),
                    ..StanceSequences::default()
                },
                StanceSequences {
                    tags: BLUNDERBUSS.to_owned(),
                    alt_start: Some(SlotsChange {
                        third_state: Some(AIM_STATE),
                        third_sequence: Some(AIM_SEQUENCE),
                        marker: Some(AIM_MARKER),
                        ..SlotsChange::default()
                    }),
                    alt_end: Some(SlotsChange {
                        third_state: Some(0),
                        third_sequence: Some(0),
                        marker: Some(0),
                        ..SlotsChange::default()
                    }),
                    alt_fire: Some(SlotsChange {
                        second_sequence: Some(SHOT_SEQUENCE),
                        marker: Some(SHOT_MARKER),
                        ..SlotsChange::default()
                    }),
                    reload: Some(SlotsChange {
                        second_state: Some(RELOAD_STATE),
                        second_sequence: Some(RELOAD_SEQUENCE),
                        ..SlotsChange::default()
                    }),
                    ..StanceSequences::default()
                },
            ],
            vec![FollowUp {
                client: 5055,
                server: 4603,
                at_release: false,
                seen: 13,
            }],
        )
    }

    fn weapon(tags: &str, drawn: bool) -> Option<HeldWeapon> {
        Some(HeldWeapon {
            stance: stance_key(tags),
            drawn,
            empty: false,
        })
    }

    fn attack_record(counter: u16) -> SlayerRecord {
        SlayerRecord {
            action: Some(SlayerInput {
                sequence: 5055,
                state: 0x000a,
                second_state: 0x002c,
                second_sequence: 0,
            }),
            intent: None,
            ..quiet_record(counter, Intent::default())
        }
    }

    fn quiet_record(counter: u16, intent: Intent) -> SlayerRecord {
        SlayerRecord {
            counter,
            frame_s: FRAME_S,
            action: None,
            intent: Some(intent),
            attack: AttackKey::default(),
            weapon: None,
            swimming: false,
            falling: false,
        }
    }

    fn keyed(counter: u16, intent: Intent, attack: AttackKey, tags: &str) -> SlayerRecord {
        SlayerRecord {
            attack,
            weapon: weapon(tags, true),
            ..quiet_record(counter, intent)
        }
    }

    /// The own state of the tick numbered `number`.
    fn own_state(track: &mut SlayerTrack, number: u16) -> SlayerDue {
        track.advance(number, u64::from(number) * TICK_MS);
        track.take_due()
    }

    fn spawned() -> SlayerTrack {
        let mut track = SlayerTrack::default();
        track.spawned();
        track
    }

    const PRESS: AttackKey = AttackKey {
        pressed: true,
        released: false,
    };
    const RELEASE: AttackKey = AttackKey {
        pressed: false,
        released: true,
    };
    const HELD: AttackKey = AttackKey {
        pressed: false,
        released: false,
    };

    #[test]
    fn an_action_goes_out_started_in_its_record_then_its_follow_up() {
        const ACTION_RECORD: u16 = 0x15c0;
        const NEWEST: u16 = ACTION_RECORD + 1;
        let table = table();
        let mut track = spawned();
        track.record(attack_record(ACTION_RECORD), &table);
        track.record(quiet_record(NEWEST, Intent::default()), &table);
        let started = own_state(&mut track, NEWEST).state.expect("the action");
        assert_eq!(
            (started.state, started.started, started.sequence),
            (None, Some(ACTION_RECORD), 5055),
            "started in the record that named it; retail echoes no state without a second-slot \
             action"
        );
        let follow_up = own_state(&mut track, NEWEST).state.expect("the follow-up");
        assert_eq!(
            (follow_up.state, follow_up.started, follow_up.sequence),
            (None, Some(NEWEST), 4603)
        );
        assert_eq!(
            own_state(&mut track, NEWEST).state,
            Some(follow_up),
            "once more"
        );
        assert_eq!(own_state(&mut track, NEWEST).state, None);
        assert_eq!(track.current(), Some(follow_up), "the others still see it");
    }

    #[test]
    fn a_tap_starts_the_light_attack_at_the_release() {
        // Retail newchar: press at 0x6ba, release at 0x6bd, the server's `14=06bd 15=8f07
        // 17=06bd`; runs 13 and 15 sent nothing and their clients never attacked.
        let table = table();
        let mut track = spawned();
        let still = Intent::default();
        track.record(keyed(0x6ba, still, PRESS, GREATSWORD), &table);
        assert_eq!(
            own_state(&mut track, 0x6ba).state,
            None,
            "nothing at the press"
        );
        track.record(keyed(0x6bb, still, HELD, GREATSWORD), &table);
        track.record(keyed(0x6bd, still, RELEASE, GREATSWORD), &table);
        let light = own_state(&mut track, 0x6bd)
            .state
            .expect("the light attack");
        assert_eq!(
            (light.state, light.started, light.sequence),
            (None, Some(0x6bd), LIGHT)
        );
        assert_eq!(
            light.second.map(|second| (second.started, second.playing)),
            Some((0x6bd, None))
        );
    }

    #[test]
    fn a_late_draw_does_not_replace_the_light_attack() {
        /// The draw the live table sends when the weapon reaches the hand.
        const DRAW_STATE: u64 = 44;
        const DRAW_SEQUENCE: u64 = 2310;
        let table = PlayerSequences::of_parts(
            HOLD_RECORDS,
            vec![StanceSequences {
                tags: GREATSWORD.to_owned(),
                light: Some(LIGHT),
                draw: Some(StatePlay {
                    state: DRAW_STATE,
                    sequence: DRAW_SEQUENCE,
                }),
                ..StanceSequences::default()
            }],
            vec![],
        );
        let mut track = spawned();
        let still = Intent::default();
        let sheathed = |counter, attack| SlayerRecord {
            weapon: weapon(GREATSWORD, false),
            ..keyed(counter, still, attack, GREATSWORD)
        };
        track.record(sheathed(0x6ba, PRESS), &table);
        track.record(sheathed(0x6bd, RELEASE), &table);
        let light = own_state(&mut track, 0x6bd)
            .state
            .expect("the light attack");
        assert_eq!(light.sequence, LIGHT);
        track.record(
            SlayerRecord {
                weapon: weapon(GREATSWORD, true),
                ..quiet_record(0x6be, still)
            },
            &table,
        );
        let after = own_state(&mut track, 0x6be)
            .state
            .expect("the attack stays");
        assert_eq!(after.sequence, LIGHT, "a draw after the tap is not a walk");
    }

    #[test]
    fn a_hold_starts_the_charge_and_its_release_no_light() {
        let table = table();
        let mut track = spawned();
        let still = Intent::default();
        track.record(keyed(100, still, PRESS, GREATSWORD), &table);
        for counter in 101..100 + HOLD_RECORDS {
            track.record(keyed(counter, still, HELD, GREATSWORD), &table);
        }
        assert_eq!(own_state(&mut track, 100 + HOLD_RECORDS - 1).state, None);
        own_state(&mut track, 100 + HOLD_RECORDS - 1);
        track.record(keyed(100 + HOLD_RECORDS, still, HELD, GREATSWORD), &table);
        let charge = own_state(&mut track, 100 + HOLD_RECORDS)
            .state
            .expect("the charge");
        assert_eq!(
            (charge.started, charge.sequence),
            (Some(100 + HOLD_RECORDS), CHARGE)
        );
        own_state(&mut track, 100 + HOLD_RECORDS);
        track.record(
            keyed(100 + HOLD_RECORDS + 5, still, RELEASE, GREATSWORD),
            &table,
        );
        assert_eq!(
            own_state(&mut track, 100 + HOLD_RECORDS + 5).state,
            None,
            "the heavy attack is the client's"
        );
    }

    #[test]
    fn a_tap_the_client_answered_gets_no_light_attack() {
        let table = table();
        let mut track = spawned();
        let still = Intent::default();
        track.record(keyed(10, still, PRESS, GREATSWORD), &table);
        track.record(
            SlayerRecord {
                weapon: weapon(GREATSWORD, true),
                ..attack_record(11)
            },
            &table,
        );
        track.record(keyed(12, still, RELEASE, GREATSWORD), &table);
        let due = own_state(&mut track, 12).state.expect("the client's");
        assert_eq!(due.sequence, 5055);
    }

    #[test]
    fn the_right_click_blocks_and_aims_and_ends_at_the_release() {
        let table = table();
        let block = Intent {
            block: true,
            ..Intent::default()
        };
        let mut track = spawned();
        track.record(keyed(20, block, HELD, GREATSWORD), &table);
        let due = own_state(&mut track, 20);
        let slots = due.slots.expect("the block");
        assert_eq!(slots.second.sequence, Some(BLOCK_SEQUENCE));
        assert_eq!(slots.second.started, None, "retail sends child 18 alone");
        assert_eq!(own_state(&mut track, 20).slots, Some(slots), "once more");
        assert_eq!(own_state(&mut track, 20).slots, None);
        let mut track = spawned();
        track.record(keyed(0x54ad, block, HELD, BLUNDERBUSS), &table);
        let aim = own_state(&mut track, 0x54ad).slots.expect("the aim");
        assert_eq!(
            aim.third,
            SlotChange {
                state: Some(AIM_STATE),
                started: Some(0x54ad),
                sequence: Some(AIM_SEQUENCE),
            }
        );
        assert_eq!(aim.marker, Some(AIM_MARKER));
        track.record(keyed(0x54b0, Intent::default(), HELD, BLUNDERBUSS), &table);
        let end = own_state(&mut track, 0x54b0).slots.expect("the end");
        assert_eq!(
            (
                end.third.state,
                end.third.started,
                end.third.sequence,
                end.marker
            ),
            (Some(0), Some(0x54b0), Some(0), Some(0))
        );
        let mut sheathed = spawned();
        sheathed.record(
            SlayerRecord {
                weapon: weapon(BLUNDERBUSS, false),
                ..quiet_record(5, block)
            },
            &table,
        );
        assert_eq!(
            own_state(&mut sheathed, 5).slots,
            None,
            "no weapon in the hand"
        );
    }

    #[test]
    fn the_players_who_see_one_get_its_upper_slots_while_its_state_echoes_them() {
        let table = table();
        let aim = Intent {
            block: true,
            ..Intent::default()
        };
        let mut track = spawned();
        track.record(keyed(0x54ad, aim, HELD, BLUNDERBUSS), &table);
        let own = own_state(&mut track, 0x54ad).slots.expect("the aim");
        let remote = track
            .remote_upper()
            .expect("kept after the own state took it");
        assert_eq!(remote.slots, own);
        assert!(remote.after_state, "the aim came after the newest state");
        own_state(&mut track, 0x54ad);
        assert_eq!(
            track.remote_upper().map(|upper| upper.slots),
            Some(own),
            "the echo"
        );
        own_state(&mut track, 0x54ad);
        assert_eq!(track.remote_upper(), None, "the echo is over");
        track.record(attack_record(0x54b0), &table);
        own_state(&mut track, 0x54b0);
        track.record(keyed(0x54b1, Intent::default(), HELD, BLUNDERBUSS), &table);
        own_state(&mut track, 0x54b1);
        assert!(
            track.remote_upper().is_some_and(|upper| upper.after_state),
            "the aim's end came after the attack"
        );
    }

    #[test]
    fn an_aimed_press_shoots_and_the_first_press_after_the_reload_time_reloads() {
        let table = table();
        let aim = Intent {
            block: true,
            ..Intent::default()
        };
        let gun = |counter: u16, attack: AttackKey, empty: bool| SlayerRecord {
            weapon: Some(HeldWeapon {
                empty,
                ..weapon(BLUNDERBUSS, true).expect("a gun")
            }),
            ..keyed(counter, aim, attack, BLUNDERBUSS)
        };
        let mut track = spawned();
        track.record(gun(0x5550, HELD, false), &table);
        own_state(&mut track, 0x5550);
        own_state(&mut track, 0x5550);
        track.record(gun(0x5551, PRESS, false), &table);
        let shot = own_state(&mut track, 0x5551).slots.expect("the aimed shot");
        assert_eq!(
            (
                shot.second.state,
                shot.second.started,
                shot.second.sequence,
                shot.marker
            ),
            (None, None, Some(SHOT_SEQUENCE), Some(SHOT_MARKER)),
            "retail `18=5e 48=29`: child 18 alone in the second slot"
        );
        own_state(&mut track, 0x5551);
        track.record(gun(0x5552, RELEASE, true), &table);
        track.record(gun(0x5553, PRESS, true), &table);
        assert_eq!(own_state(&mut track, 0x5553).slots, None, "empty: nothing");
        track.record(gun(0x5554, RELEASE, false), &table);
        assert_eq!(
            own_state(&mut track, 0x5554).slots,
            None,
            "a load is no reload"
        );
        track.record(gun(0x557a, PRESS, false), &table);
        let reload = own_state(&mut track, 0x557a).slots.expect("the reload");
        assert_eq!(
            (
                reload.second.state,
                reload.second.started,
                reload.second.sequence
            ),
            (Some(RELOAD_STATE), Some(0x557a), Some(RELOAD_SEQUENCE))
        );
        own_state(&mut track, 0x557a);
        track.record(gun(0x557b, RELEASE, false), &table);
        assert_eq!(
            own_state(&mut track, 0x557b)
                .state
                .map(|state| state.started),
            None,
            "the reload's release starts no attack"
        );
        track.record(gun(0x5584, PRESS, false), &table);
        let again = own_state(&mut track, 0x5584).slots.expect("shoots again");
        assert_eq!(again.second.sequence, Some(SHOT_SEQUENCE));
        let mut hip = spawned();
        hip.record(keyed(0x5600, Intent::default(), PRESS, BLUNDERBUSS), &table);
        assert_eq!(
            own_state(&mut hip, 0x5600).slots,
            None,
            "no aimed shot from the hip"
        );
    }

    #[test]
    fn no_attack_starts_while_aiming() {
        let table = table();
        let aim = Intent {
            block: true,
            ..Intent::default()
        };
        let mut track = spawned();
        track.record(keyed(30, aim, PRESS, GREATSWORD), &table);
        track.record(keyed(31, aim, RELEASE, GREATSWORD), &table);
        let due = own_state(&mut track, 31);
        assert_ne!(due.state.map(|state| state.sequence), Some(LIGHT));
    }

    #[test]
    fn the_drawn_weapon_walks_and_dodges_with_its_stance() {
        let table = table();
        let forward = Intent::of_keys(true, false, false, false);
        let mut track = spawned();
        track.record(keyed(40, forward, HELD, GREATSWORD), &table);
        let start = own_state(&mut track, 40).state.expect("a start");
        assert_eq!(
            (start.state, start.sequence),
            (Some(DRAWN_START.state), DRAWN_START.sequence)
        );
        let dodge = Intent {
            dodge: true,
            ..Intent::of_keys(false, true, false, false)
        };
        let mut track = spawned();
        track.record(keyed(50, dodge, HELD, GREATSWORD), &table);
        let roll = own_state(&mut track, 50).state.expect("the dodge");
        assert_eq!(
            (roll.state, roll.started, roll.sequence),
            (Some(DODGE_BACK.state), Some(50), DODGE_BACK.sequence)
        );
        track.record(keyed(51, dodge, HELD, GREATSWORD), &table);
        own_state(&mut track, 51);
        assert_ne!(
            own_state(&mut track, 51)
                .state
                .and_then(|state| state.started),
            Some(51),
            "a held key dodges once"
        );
        let block_walk = Intent {
            block: true,
            ..forward
        };
        let mut track = spawned();
        track.record(keyed(60, block_walk, HELD, GREATSWORD), &table);
        let walk = own_state(&mut track, 60).state.expect("the block walk");
        assert_eq!(
            (walk.state, walk.sequence),
            (Some(BLOCK_WALK.state), BLOCK_WALK.sequence)
        );
    }

    #[test]
    fn the_sequence_time_counts_the_records_since_the_action() {
        // Two records after the action's record: 3 frames in, from an absolute part of one.
        let table = table();
        let mut track = spawned();
        track.record(attack_record(10), &table);
        track.record(quiet_record(11, Intent::default()), &table);
        track.record(quiet_record(12, Intent::default()), &table);
        let time = own_state(&mut track, 12).time.expect("time");
        let mut clock = SequenceClock::default();
        assert_eq!(
            time,
            clock.time(3.0 * FRAME_S, Some(FRAME_S), 12 * TICK_MS),
            "{time:?}"
        );
        assert!(time.abs.is_some(), "a start sends its absolute part");
    }

    #[test]
    fn no_time_goes_before_the_first_own_state() {
        let table = table();
        let mut track = SlayerTrack::default();
        track.record(quiet_record(1, Intent::default()), &table);
        assert_eq!(own_state(&mut track, 1), SlayerDue::default());
        let mut track = spawned();
        track.record(quiet_record(3, Intent::default()), &table);
        assert_eq!(
            own_state(&mut track, 3).time.map(|time| time.rel),
            Some(0x83),
            "the first state's two steps and one more record"
        );
    }

    #[test]
    fn moving_starts_stops_and_comes_to_rest_with_the_retail_sequences() {
        let table = table();
        let forward = Intent::of_keys(true, false, false, false);
        let mut track = spawned();
        let mut counter = 100;
        let mut step = |track: &mut SlayerTrack, intent: Intent| {
            counter += 1;
            track.record(quiet_record(counter, intent), &table);
            own_state(track, counter).state
        };
        let start = step(&mut track, forward).expect("a start");
        assert_eq!(
            (start.state, start.sequence),
            (Some(movement::START_STATE), movement::START_SEQUENCE)
        );
        // The echo, then nothing while running.
        assert_eq!(step(&mut track, forward), Some(start));
        assert_eq!(step(&mut track, forward), None);
        let stop = step(&mut track, Intent::default()).expect("a stop");
        assert_eq!(
            (stop.state, stop.sequence),
            (Some(movement::MOVE_STATE), movement::STOP_SEQUENCE)
        );
        let idle = (0..movement::STOP_RECORDS)
            .find_map(|_| {
                step(&mut track, Intent::default())
                    .filter(|state| state.state == Some(movement::IDLE_STATE))
            })
            .expect("standing still");
        assert_eq!(idle.sequence, movement::IDLE_SEQUENCE);
        assert_eq!(
            idle.started
                .zip(stop.started)
                .map(|(idle, stop)| idle.wrapping_sub(stop)),
            Some(movement::STOP_RECORDS)
        );
        let back = step(&mut track, Intent::of_keys(false, true, false, false)).expect("back");
        assert_eq!(
            (back.state, back.sequence),
            (Some(movement::MOVE_STATE), movement::BACK_START_SEQUENCE)
        );
        step(&mut track, Intent::default());
        let again = step(&mut track, forward).expect("moving again");
        assert_eq!(again.sequence, movement::RESTART_SEQUENCE);
    }

    #[test]
    fn the_movement_sequences_are_the_retail_bytes() {
        let bytes = |sequence: u64| {
            nw_messages::wire::Writer::new()
                .prefixed(sequence)
                .expect("prefix")
                .finish()
        };
        for (sequence, retail) in [
            (movement::START_SEQUENCE, [0xaa, 0x2b]),
            (movement::BACK_START_SEQUENCE, [0x98, 0x14]),
            (movement::STOP_SEQUENCE, [0xbd, 0x15]),
            (movement::RESTART_SEQUENCE, [0xb6, 0x25]),
            (movement::IDLE_SEQUENCE, [0xb9, 0x10]),
            (swim::IDLE_SEQUENCE, [0xa0, 0x21]),
            (swim::FLOAT_SEQUENCE, [0xb6, 0x21]),
            (swim::STROKE_SEQUENCE, [0xa5, 0x2e]),
            (swim::NEXT_STROKE_SEQUENCE, [0x97, 0x2e]),
            (swim::FALL_SEQUENCE, [0x9d, 0x3c]),
            (loadout::OPEN_SEQUENCE, [0x96, 0x2b]),
            (loadout::LOOP_SEQUENCE, [0x89, 0x0a]),
            (loadout::CLOSE_SEQUENCE, [0x8b, 0x2e]),
        ] {
            assert_eq!(bytes(sequence), retail, "{sequence}");
        }
    }

    #[test]
    fn the_server_sends_the_fall_the_entry_and_the_swim_strokes_like_retail() {
        let table = table();
        let forward = Intent::of_keys(true, false, false, false);
        let mut track = spawned();
        let mut counter = 200;
        let mut step = |track: &mut SlayerTrack, intent: Intent, swimming, falling| {
            counter += 1;
            track.record(
                SlayerRecord {
                    swimming,
                    falling,
                    ..quiet_record(counter, intent)
                },
                &table,
            );
            own_state(track, counter).state
        };
        let fall = step(&mut track, Intent::default(), false, true).expect("a fall");
        assert_eq!(
            (fall.state, fall.sequence),
            (Some(swim::FALL_STATE), swim::FALL_SEQUENCE)
        );
        assert_eq!(
            step(&mut track, Intent::default(), false, true),
            Some(fall),
            "the fall echoes, it does not start again"
        );
        let entry = step(&mut track, Intent::default(), true, false).expect("the entry");
        assert_eq!(
            (entry.state, entry.sequence),
            (Some(swim::STATE), swim::IDLE_SEQUENCE)
        );
        // Each change after the entry is the sequence alone: no state, no start (child 15 only).
        let mut next_change = |track: &mut SlayerTrack, intent: Intent, last: u64| {
            for record in 1..1_000_u32 {
                let state = step(track, intent, true, false);
                if let Some(state) = state.filter(|state| state.sequence != last) {
                    assert_eq!((state.state, state.started), (None, None), "{state:?}");
                    #[allow(clippy::cast_precision_loss, reason = "a few hundred records")]
                    return (state.sequence, record as f32 * FRAME_S);
                }
            }
            panic!("no change after {last}");
        };
        let (float, after_s) = next_change(&mut track, Intent::default(), swim::IDLE_SEQUENCE);
        assert_eq!(float, swim::FLOAT_SEQUENCE);
        assert!((after_s - swim::ENTRY_S).abs() <= FRAME_S, "{after_s}");
        let (stroke, at_s) = next_change(&mut track, forward, float);
        assert_eq!(
            (stroke, at_s),
            (swim::STROKE_SEQUENCE, FRAME_S),
            "at the press"
        );
        let (next, after_s) = next_change(&mut track, forward, stroke);
        assert_eq!(next, swim::NEXT_STROKE_SEQUENCE);
        assert!((after_s - swim::STROKE_S).abs() <= FRAME_S, "{after_s}");
        let (again, after_s) = next_change(&mut track, forward, next);
        assert_eq!(again, swim::STROKE_SEQUENCE);
        assert!(
            (after_s - swim::NEXT_STROKE_S).abs() <= FRAME_S,
            "{after_s}"
        );
        let (stop, at_s) = next_change(&mut track, Intent::default(), again);
        assert_eq!(
            (stop, at_s),
            (swim::FLOAT_SEQUENCE, FRAME_S),
            "at the release"
        );
        let out = step(&mut track, forward, false, false).expect("a walk out of the water");
        assert!(
            out.state.is_some(),
            "on land the movement states come back: {out:?}"
        );
    }

    #[test]
    fn a_pull_plays_the_pulled_state_then_stands_the_player_still() {
        /// The retail pull's own state number (`14=3163`), as this test's tick number.
        const PULLED_IN: u16 = 0x3163;
        let mut track = spawned();
        let pulled_at_ms = u64::from(PULLED_IN - 1) * TICK_MS;
        track.pulled(pulled_at_ms);
        let state = own_state(&mut track, PULLED_IN).state;
        assert_eq!(state, Some(pulled_state(PULLED_IN)));
        assert_eq!(
            own_state(&mut track, PULLED_IN + 1).state,
            state,
            "it echoes once"
        );
        let ends_at = PULLED_IN - 1 + u16::try_from(pull::LENGTH_MS / TICK_MS).expect("ticks") + 1;
        let mut idle = None;
        for number in PULLED_IN + 2..=ends_at {
            idle = idle.or(own_state(&mut track, number).state);
        }
        assert_eq!(
            idle.map(|idle| (idle.state, idle.sequence)),
            Some((Some(movement::IDLE_STATE), movement::IDLE_SEQUENCE)),
            "standing still once the pull is over"
        );
    }

    #[test]
    fn a_fast_travel_kneels_in_the_state_that_goes_on_and_a_key_moves_again() {
        /// The retail kneel's own state number (`14=3fcc`), as this test's tick number.
        const KNEELS_IN: u16 = 0x3fcc;
        let table = table();
        let forward = Intent::of_keys(true, false, false, false);
        let mut track = spawned();
        // A stop in progress must not stand the player over the kneel.
        track.record(quiet_record(KNEELS_IN - 3, forward), &table);
        own_state(&mut track, KNEELS_IN - 3);
        track.record(quiet_record(KNEELS_IN - 2, Intent::default()), &table);
        own_state(&mut track, KNEELS_IN - 2);
        own_state(&mut track, KNEELS_IN - 1);
        track.kneel(KneelFrom::State);
        let kneel = own_state(&mut track, KNEELS_IN).state;
        assert_eq!(
            kneel,
            Some(SlayerState {
                state: None,
                started: Some(KNEELS_IN),
                sequence: kneel::SEQUENCE,
                second: None,
            })
        );
        assert_eq!(
            own_state(&mut track, KNEELS_IN + 1).state,
            kneel,
            "it echoes once"
        );
        for number in KNEELS_IN + 2..KNEELS_IN + 2 + movement::STOP_RECORDS {
            track.record(quiet_record(number, Intent::default()), &table);
            assert_eq!(
                own_state(&mut track, number).state,
                None,
                "kneeling at {number}"
            );
        }
        let moved_in = KNEELS_IN + 2 + movement::STOP_RECORDS;
        track.record(quiet_record(moved_in, forward), &table);
        let moved = own_state(&mut track, moved_in).state.expect("a start");
        assert_eq!(
            (moved.state, moved.sequence),
            (Some(movement::START_STATE), movement::START_SEQUENCE)
        );
    }

    #[test]
    fn the_end_of_a_channel_stands_the_player_still_unless_it_moves() {
        /// The retail arrival's state (`13=1f 14=409b 15=b910 16=2d 17=409b 18=00`).
        const STANDS_IN: u16 = 0x409b;
        let table = table();
        let mut track = spawned();
        track.kneel(KneelFrom::State);
        own_state(&mut track, STANDS_IN - 2);
        track.stand();
        assert_eq!(
            own_state(&mut track, STANDS_IN).state,
            Some(SlayerState {
                state: Some(movement::IDLE_STATE),
                started: Some(STANDS_IN),
                sequence: movement::IDLE_SEQUENCE,
                second: Some(SecondSlayerState {
                    started: STANDS_IN,
                    playing: Some((loadout::CLOSED_STATE, 0)),
                    stopped: false,
                }),
            })
        );
        track.stand();
        let moved_in = STANDS_IN + 5;
        track.record(
            quiet_record(moved_in, Intent::of_keys(true, false, false, false)),
            &table,
        );
        let moved = own_state(&mut track, moved_in).state.expect("a start");
        assert_eq!(
            moved.state,
            Some(movement::START_STATE),
            "a cancel by a key moves"
        );
    }

    #[test]
    fn a_rider_kneels_standing_still() {
        const KNEELS_IN: u16 = 0x46aa;
        let mut track = spawned();
        track.kneel(KneelFrom::Dismount);
        let kneel = own_state(&mut track, KNEELS_IN).state.expect("a kneel");
        assert_eq!(
            (kneel.state, kneel.started, kneel.sequence),
            (Some(movement::IDLE_STATE), Some(KNEELS_IN), kneel::SEQUENCE)
        );
    }

    #[test]
    fn the_inventory_slot_opens_loops_closes_and_clears_like_retail() {
        const OPENED_IN: u16 = 0x11a9;
        const CLOSED_IN: u16 = 0x1264;
        const CLEARED_IN: u16 = 0x1290;
        let mut track = spawned();
        let mut number = 1;
        let mut tick = |track: &mut SlayerTrack| {
            number += 1;
            own_state(track, number).slots.map(|slots| {
                (
                    slots.second.state,
                    slots.second.started,
                    slots.second.sequence,
                )
            })
        };
        let open = Some((
            Some(loadout::OPEN_STATE),
            Some(OPENED_IN),
            Some(loadout::OPEN_SEQUENCE),
        ));
        track.open_loadout(OPENED_IN, 2 * TICK_MS);
        assert_eq!([tick(&mut track), tick(&mut track)], [open, open]);
        assert_eq!(tick(&mut track), None, "the open echoes, then stops");
        let mut looping = None;
        for _ in 0..loadout::LOOP_MS / TICK_MS + 1 {
            looping = looping.or(tick(&mut track));
        }
        assert_eq!(
            looping,
            Some((
                Some(loadout::OPEN_STATE),
                Some(OPENED_IN),
                Some(loadout::LOOP_SEQUENCE)
            ))
        );
        track.close_loadout(CLOSED_IN);
        let close = Some((None, Some(CLOSED_IN), Some(loadout::CLOSE_SEQUENCE)));
        assert_eq!([tick(&mut track), tick(&mut track)], [close, close]);
        assert_eq!(tick(&mut track), None, "no clear before the second close");
        track.close_loadout(CLEARED_IN);
        let clear = Some((Some(loadout::CLOSED_STATE), Some(CLEARED_IN), Some(0)));
        assert_eq!([tick(&mut track), tick(&mut track)], [clear, clear]);
        assert_eq!(tick(&mut track), None, "the slot ends");
        track.close_loadout(CLEARED_IN + 1);
        assert_eq!(tick(&mut track), None, "a close with no open sends nothing");
    }
}
