// SPDX-License-Identifier: AGPL-3.0-or-later
//! `content-tool ai-trace`: what the retail mobs of one capture ledger did, for the behavior-tree
//! AI. One trace per mob spawn lifetime: its card asset and creature slice, its vitals id, and its
//! `ALCReplicatedState` (type 11) samples in time order: state number, position, facing, the turn
//! the AI still wants, and each new FullBody scope entry by fragment and action name. Next to the
//! mobs: the own player's position track and the hits the player took.
//!
//! Labels: **Observed**, **Inferred**, **Unknown**.
//!
//! - Mob (Observed, the rule of `ftue_entities`): a bundle group whose spawn card names an asset
//!   other than the player slice and a `GdeRef`, with a vitals id in the same group. Its lifetime
//!   is one `(connection context, interest)` slot from that card until the slot is stopped by the
//!   bundle index block, another mob's card takes the slot, the connection starts again, or the
//!   level changes. Groups of the own player's interest 1 (bundle note) give only the player
//!   track: the newchar capture has a card there with an asset outside the creature table
//!   (1,041,878 ms on). A card alone is not taken: the byte-by-byte search finds lone cards in
//!   the middle of groups it cannot read (Observed: boar cards "at" interest 1).
//! - Group walk: [`BundlePayload::decode_known`] reads the members whose class `nw-messages`
//!   reads, from the payload start; a member of a class that only [`extra_members`] reads is
//!   passed with it, and the walk goes on in the same group. Where neither reads a member, the
//!   walk looks for the next group one byte at a time and takes it when its first group is whole
//!   and ends at a group boundary (the rule of `ftue_entities`, whose walker is private, so this
//!   module has its own smallest one).
//! - State number (Observed: client text "Absolute portion in delta compression"; own-player
//!   position note): `idAbs` (child 25) is the high byte, `idRel` (child 0) the low byte; a child
//!   that a state does not send keeps its last value.
//! - Position: the last `pos10` (child 27, read by [`AlcState::read`]) plus the last
//!   `worldPosRel` (child 10) offset of [`nw_messages::offset_metres`] per axis (4/255 m per step
//!   from `0x7f`, `0xff` being no offset). The rule is Observed to hold: see the player hits below.
//! - Facing (Observed, rotation note; `nw_messages::alc::rotation_bytes` re-encodes all 3,648
//!   retail values): the `rotation` quat (child 11) in smallest-three form; yaw 0 faces north
//!   (+y) and a positive yaw turns to the west (counter-clockwise seen from above).
//! - Turn wanted (Observed name, creature-animation note): `aiAngleToDesiredFacing` (child 35),
//!   a big-endian `f32` in radians; its sign convention is Unknown.
//! - FullBody entry (Inferred from `nw_messages::alc::MobAnimation`, which writes them so, and
//!   Observed to fit: in the newchar capture the entry count equals the set bits of the mask in
//!   every mob blob but 178): the `scopeInfoBlob` (child 33) holds one entry per set bit of
//!   `scopeData` (child 32), in bit order; the FullBody entry is the one of the creature's
//!   `FullBody` scope bit. For the other blobs see [`AlcTrack::full_body_entry`]. Fragment `n` is
//!   `fragments[n - 1]` and action `n` is `actions[n - 1]` of the creature row.
//! - Player position: the own player's ALC states (interest 1, component 16; payload note), by
//!   the same position rule as a mob. This is the place the retail server itself keeps for the
//!   player, so its AI measures from it. The bit-7 position of the client input (type 1589) is
//!   the third-person camera, 2.9 to 4.75 m behind the body (Observed, `bins/sim/src/camera.rs`
//!   note), so it is not used. The mob position rule matches the attacker position of all 26
//!   retail `OnDamage` hits of the newchar capture within 0.00 to 1.33 m (median 0.055 m;
//!   Observed), so the position rule above is Observed to hold, with its step Inferred.
//! - Player hits (Observed, swing note in `nw_messages::combat`): every `OnDamage` (type 3601)
//!   names the attacker's facet entity and where the attacker stood; the attacker is the mob whose
//!   card `GdeRef` has that facet entity.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use nw_capture::{Decoder, SERVER_TO_CLIENT, ledger_records};
use nw_content::creature_animations::CreatureRow;
use nw_content::ftue_entities::{FACET_LEN, GDE_REF_LEN, facet_of};
use nw_messages::{
    AlcState, BodyCodec, BundleMember, BundlePayload, DEPLETED_GATHERABLE,
    GdeMetadataReplicatedState, InputPosition, LEVEL_INFO_CHANGED_MSG_TYPE_INDEX,
    LOCAL_PLAYER_INTEREST, MemberState, ON_DAMAGE_TYPE_INDEX, OnDamageMsg, PLAYER_SLICE_ASSET,
    POS_REL_CHILD, REPLICATED_STATE_BUNDLE_TYPE_INDEX, Reader, RemoteFlags, ReplicatedStateBundle,
    ScopeEntry, Writer,
};
use nw_reflect::decode_type_id;
use serde::Serialize;

use crate::extra_members;

/// Where `ai-trace` writes the trace of the retail newchar capture of 2026-09-18.
pub const DEFAULT_TRACE_PATH: &str =
    "local-data/scratch-work/btree/trace/ai-trace-newchar-retail-2026-09-18.json";

/// The low 16 bits of a group integer are the interest id (bundle note).
const INTEREST_MASK: u64 = 0xFFFF;
/// The component of the own player's ALC state (payload note: always 16 in the captures).
const OWN_ALC_COMPONENT: u64 = 16;
/// ALC slot-0 child `idRel`: the low byte of the state number.
const ID_REL_CHILD: usize = 0;
/// ALC slot-0 child `rotation`: the facing quat.
const ROTATION_CHILD: usize = 11;
/// ALC slot-0 child `idAbs`: the high byte of the state number.
const ID_ABS_CHILD: usize = 25;
/// ALC slot-0 child `scopeData`: the mask of the active scopes.
const SCOPE_DATA_CHILD: usize = 32;
/// ALC slot-0 child `scopeInfoBlob`: the scope entries.
const SCOPE_INFO_CHILD: usize = 33;
/// ALC slot-0 child `aiAngleToDesiredFacing` (radians).
const FACING_GAP_CHILD: usize = 35;
/// Axes of a position.
const AXES: usize = 3;
/// One quat byte: `byte × QUAT_STEP - QUAT_OFFSET` (`FUN_14087a8d0`: sqrt(2) / 255 and
/// 1 / sqrt(2)).
const QUAT_STEP: f32 = std::f32::consts::SQRT_2 / 255.0;
const QUAT_OFFSET: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// Parts of a quat (x, y, z, w).
const QUAT_PARTS: usize = 4;
/// Quat part indexes.
const QUAT_X: usize = 0;
const QUAT_Y: usize = 1;
const QUAT_Z: usize = 2;
const QUAT_W: usize = 3;
/// The bit of a quat tag where the index of the derived (largest) part starts, and its mask.
const QUAT_DERIVED_SHIFT: u8 = 5;
const QUAT_DERIVED_MASK: u8 = 0b11;
/// Set in a quat tag when the derived part is negative.
const QUAT_NEGATIVE: u8 = 1;
/// The quat tag flag of part 0 being zero; part `i` is this flag shifted by `i`.
const QUAT_ZERO_FLAG: u8 = 0b10;
/// The scope whose entry is the mob's main animation.
const FULL_BODY_SCOPE: &str = "FullBody";

/// A bad input or output file.
#[derive(Debug, thiserror::Error)]
pub enum AiTraceError {
    /// A file cannot be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The creature table is not its JSON, or the trace cannot be written as JSON.
    #[error("ai trace JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// The trace of one ledger.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AiTrace {
    /// One entry per mob spawn lifetime, in order of the first card.
    pub mobs: Vec<MobTrace>,
    /// The own player's positions from its own ALC states, in time order.
    pub player: Vec<PlayerSample>,
    /// The hits the own player took, in time order.
    pub player_hits: Vec<PlayerHit>,
    /// The own player's mount states, in time order.
    pub player_mount: Vec<MountSample>,
    /// Counts of the walk.
    pub stats: TraceStats,
}

/// One mob spawn lifetime.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MobTrace {
    /// The connection context of the ledger records.
    pub context: u64,
    /// The interest id (entity slot) of its groups.
    pub interest: u16,
    /// The card asset, hex.
    pub asset: String,
    /// The creature slice, when the creature table has the asset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slice: Option<String>,
    /// The vitals id of its card group.
    pub vitals_id: u32,
    /// The card `GdeRef`, hex.
    pub gde_ref: String,
    /// Time of its card, in ms from the first ledger record.
    pub first_ms: u64,
    /// Time of its last group, in ms from the first ledger record.
    pub last_ms: u64,
    /// Time of the first group whose gatherable controller is Depleted (a skinned corpse), in ms
    /// from the first ledger record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depleted_ms: Option<u64>,
    /// Its ALC states, in time order.
    pub samples: Vec<MobSample>,
}

/// One ALC state of a mob.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MobSample {
    /// Ms from the first ledger record.
    pub time_ms: u64,
    /// The state came in a group with the mob's card: a spawn state (Observed: state number 0,
    /// `scopeData` 0 and no scope entry for the beach boars).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub card_state: bool,
    /// The state number, once an `idAbs` was sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_id: Option<u16>,
    /// Where it is, in metres (x east, y north, z up), once a `pos10` was sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; AXES]>,
    /// Its facing in radians, when this state sends one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    /// `aiAngleToDesiredFacing` in radians, when this state sends it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facing_gap: Option<f32>,
    /// The new FullBody scope entry, when this state changes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_body: Option<FullBodyEntry>,
}

/// A FullBody scope entry with the names of its fragment and action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FullBodyEntry {
    /// The state number in which it started.
    pub start_id: u16,
    /// The fragment number.
    pub fragment: u16,
    /// The fragment name, when the creature has that many fragments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragment_name: Option<String>,
    /// The action number.
    pub action: u16,
    /// The action name, when the creature has that many actions. Observed: in the newchar
    /// capture the grid action of this name can play the entry's fragment for every entry of
    /// every creature (`nw_formats::mannequin::CageGrid::number`; the creature animation
    /// capture check).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_name: Option<String>,
}

/// One own mount state (`MB::MountComponentReplicatedState`): the children it sent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MountSample {
    /// Ms from the first ledger record.
    pub time_ms: u64,
    /// The player rides (`m_isMounted`), when sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mounted: Option<bool>,
    /// The mount dashes (`RemoteFlags::DASHING` of what others see), when sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dashing: Option<bool>,
}

/// One own player position.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PlayerSample {
    /// Ms from the first ledger record.
    pub time_ms: u64,
    /// Where the server keeps the player, in metres.
    pub position: [f32; AXES],
}

/// One hit on the own player.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlayerHit {
    /// Ms from the first ledger record.
    pub time_ms: u64,
    /// The attacker, by index in [`AiTrace::mobs`], when a card named its facet entity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mob: Option<usize>,
    /// The attacker's facet entity, hex.
    pub attacker_facet: String,
    /// Where the attacker stood, in metres.
    pub attacker_at: [f32; AXES],
    /// The damage of all items.
    pub amount: f32,
}

/// Counts of the walk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TraceStats {
    /// Bundles read.
    pub bundles: usize,
    /// Groups taken.
    pub groups: usize,
    /// Places where the walk lost the group chain and looked for the next group.
    pub resyncs: usize,
    /// Mob ALC members taken.
    pub mob_states: usize,
    /// ALC members of a mob slot with another component than the mob's ALC, left out.
    pub other_component_states: usize,
    /// Scope blobs that did not read or did not fit the scope mask.
    pub bad_scope_blobs: usize,
    /// Hits whose attacker no card named.
    pub unknown_attackers: usize,
}

/// One group of a payload: its interest and the members that were read.
#[derive(Debug, Clone, PartialEq)]
struct ReadGroup {
    interest: u16,
    members: Vec<BundleMember>,
}

/// A group whose members go on at a payload offset: its interest and the members still unread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenGroup {
    interest: u16,
    left: usize,
}

/// What one [`BundlePayload::decode_known`] pass from a payload offset read.
struct Pass {
    groups: Vec<ReadGroup>,
    /// The payload offset after the last member read.
    end: usize,
    /// The group the pass stopped in, at `end`, with the members not read yet.
    open: Option<OpenGroup>,
    /// The payload offset of the first group's first member.
    first_members_at: usize,
    /// The payload offset after the first group, when all its members were read.
    first_whole_end: Option<usize>,
}

/// The groups of `payload`. The walk starts at the payload start; a member whose class
/// `nw-messages` cannot read is passed with [`extra_members::read_body`]; when that fails too,
/// the walk looks for the next group one byte at a time ([`resync_pass`]).
fn payload_groups(payload: &[u8], stats: &mut TraceStats) -> Vec<ReadGroup> {
    let mut groups = Vec::new();
    let mut at = 0;
    let mut open = None;
    let mut in_chain = true;
    while at < payload.len() {
        let pass = if in_chain {
            pass(payload, at, open)
        } else {
            resync_pass(payload, at)
        };
        let Some(pass) = pass.filter(|pass| pass.end > at || pass.open.is_some()) else {
            if in_chain {
                stats.resyncs += 1;
            }
            (at, open, in_chain) = (at + 1, None, false);
            continue;
        };
        groups.extend(pass.groups);
        at = pass.end;
        open = pass.open;
        in_chain = true;
        let Some(mut group) = open else {
            if at < payload.len() {
                stats.resyncs += 1;
                (at, in_chain) = (at + 1, false);
            }
            continue;
        };
        match pass_extra_member(payload, at) {
            Some(next) => {
                at = next;
                group.left -= 1;
                open = (group.left > 0).then_some(group);
            }
            None => {
                stats.resyncs += 1;
                (at, open, in_chain) = (at + 1, None, false);
            }
        }
    }
    groups
}

/// The payload offset after the member at `at`, when [`extra_members`] reads its class.
fn pass_extra_member(payload: &[u8], at: usize) -> Option<usize> {
    let mut reader = Reader::new(payload.get(at..)?);
    let (_, class) = extra_members::read_member_header(&mut reader)?;
    extra_members::read_body(class, &mut reader)?;
    Some(payload.len() - reader.remaining())
}

/// A pass from `at`, inside `open` when the members of that group go on there (a group header
/// for its unread members is put before the bytes).
fn pass(payload: &[u8], at: usize, open: Option<OpenGroup>) -> Option<Pass> {
    let rest = payload.get(at..)?;
    let bytes: Cow<'_, [u8]> = match open {
        Some(group) => {
            let mut writer = Writer::new();
            writer
                .prefixed(u64::from(group.interest))
                .ok()?
                .u8(u8::try_from(group.left).ok()?)
                .bytes(rest);
            Cow::Owned(writer.finish())
        }
        None => Cow::Borrowed(rest),
    };
    // The payload offset of byte `index` of `bytes`.
    let head = bytes.len() - rest.len();
    let offset = |index: usize| at + index - head;
    let known = BundlePayload::decode_known(&bytes).ok()?;
    let mut members = known.members.into_iter().peekable();
    let mut groups = Vec::new();
    let mut cursor = 0;
    let mut first = None;
    let mut stopped_in = None;
    while cursor < bytes.len() {
        let mut header = Reader::new(&bytes[cursor..]);
        let interest = u16::try_from(header.prefixed().ok()? & INTEREST_MASK).ok()?;
        let count = usize::from(header.u8().ok()?);
        let members_at = bytes.len() - header.remaining();
        if count > 0
            && members
                .peek()
                .is_some_and(|(_, range)| range.start != members_at)
        {
            return None;
        }
        cursor = members_at;
        let mut group = ReadGroup {
            interest,
            members: Vec::with_capacity(count),
        };
        while group.members.len() < count {
            let Some((member, range)) = members.next() else {
                break;
            };
            cursor = range.end;
            group.members.push(member);
        }
        let whole = group.members.len() == count;
        // A first group without members is no group start (the boundary rule needs a member).
        first.get_or_insert((
            offset(members_at),
            (whole && count > 0).then(|| offset(cursor)),
        ));
        if !whole {
            stopped_in = Some(OpenGroup {
                interest,
                left: count - group.members.len(),
            });
        }
        if !group.members.is_empty() {
            groups.push(group);
        }
        if !whole {
            break;
        }
    }
    let (first_members_at, first_whole_end) = first.unwrap_or((at, None));
    Some(Pass {
        groups,
        end: offset(cursor),
        open: stopped_in,
        first_members_at,
        first_whole_end,
    })
}

/// A pass from `at`, which is not known to start a group: taken when its first group is whole
/// and ends at a group boundary. When a later start before its first member also gives a whole
/// first group with the same end, that pass is the real one: the bytes before it only looked
/// like a longer group integer.
fn resync_pass(payload: &[u8], at: usize) -> Option<Pass> {
    let whole = |start| {
        pass(payload, start, None).filter(|pass| {
            pass.first_whole_end
                .is_some_and(|end| is_boundary(payload, end))
        })
    };
    let found = whole(at)?;
    let later = (at + 1..found.first_members_at)
        .rev()
        .filter_map(whole)
        .find(|later| later.first_whole_end == found.first_whole_end);
    Some(later.unwrap_or(found))
}

/// Whether `at` is the end of `payload` or the start of a group whose first member header reads.
fn is_boundary(payload: &[u8], at: usize) -> bool {
    if at == payload.len() {
        return true;
    }
    let mut reader = Reader::new(&payload[at..]);
    reader.prefixed().is_ok()
        && reader.u8().is_ok_and(|count| count > 0)
        && reader.prefixed().is_ok()
        && decode_type_id(reader.rest()).is_ok()
}

/// The yaw of a `rotation` quat in smallest-three form: a tag with the index of the derived part
/// in bits 5 and 6, its sign in bit 0 and a zero flag per part in bits 1 to 4, then one byte per
/// other part that is not zero.
fn quat_yaw(bytes: &[u8]) -> Option<f32> {
    let (&tag, mut rest) = bytes.split_first()?;
    let derived = usize::from(tag >> QUAT_DERIVED_SHIFT & QUAT_DERIVED_MASK);
    let mut parts = [0.0_f32; QUAT_PARTS];
    for (index, part) in parts.iter_mut().enumerate() {
        if index == derived || tag & (QUAT_ZERO_FLAG << index) != 0 {
            continue;
        }
        let (&byte, more) = rest.split_first()?;
        *part = f32::from(byte) * QUAT_STEP - QUAT_OFFSET;
        rest = more;
    }
    let others: f32 = parts.iter().map(|part| part * part).sum();
    let size = (1.0 - others).max(0.0).sqrt();
    parts[derived] = if tag & QUAT_NEGATIVE == 0 {
        size
    } else {
        -size
    };
    let [x, y, z, w] = [parts[QUAT_X], parts[QUAT_Y], parts[QUAT_Z], parts[QUAT_W]];
    Some((2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)))
}

/// The entries of a `scopeInfoBlob`: its byte count, then entries of a `u16` start, a prefixed
/// fragment, a prefixed action, a `u16` speed and a `u8` tail.
fn scope_entries(blob: &[u8]) -> Option<Vec<ScopeEntry>> {
    let mut reader = Reader::new(blob);
    let len = usize::try_from(reader.prefixed().ok()?).ok()?;
    let mut body = Reader::new(reader.bytes(len).ok()?);
    let mut entries = Vec::new();
    while !body.is_empty() {
        let start_id = body.u16().ok()?;
        let fragment = u16::try_from(body.prefixed().ok()?).ok()?;
        let action = u16::try_from(body.prefixed().ok()?).ok()?;
        body.u16().ok()?;
        let tail = body.u8().ok()?;
        entries.push(ScopeEntry {
            start_id,
            fragment,
            action,
            tail,
        });
    }
    Some(entries)
}

/// The 1-based `number` of `names`.
fn name_of(names: &[String], number: u16) -> Option<String> {
    names.get(usize::from(number).checked_sub(1)?).cloned()
}

/// The delta-compressed ALC values of one mob.
#[derive(Debug, Default)]
struct AlcTrack {
    id_abs: Option<u8>,
    id_rel: Option<u8>,
    pos10: Option<InputPosition>,
    rel: [u8; AXES],
    scope_mask: Option<u64>,
    full_body: Option<ScopeEntry>,
}

/// A mob in view.
#[derive(Debug)]
struct LiveMob {
    /// Index in the trace.
    index: usize,
    gde_ref: [u8; GDE_REF_LEN],
    /// The component of its ALC member, once seen.
    alc_component: Option<u64>,
    alc: AlcTrack,
}

/// The walk of one ledger.
struct Tracer<'a> {
    creatures: HashMap<String, &'a CreatureRow>,
    live: HashMap<(u64, u16), LiveMob>,
    /// The own player's ALC values, by connection context.
    own: HashMap<u64, AlcTrack>,
    /// Mob index by facet entity, the newest lifetime.
    facets: HashMap<[u8; FACET_LEN], usize>,
    /// The interest of the group that carries the own mount state, once a ride was sent.
    own_mount_interest: Option<u16>,
    trace: AiTrace,
}

impl<'a> Tracer<'a> {
    fn new(creatures: &'a [CreatureRow]) -> Self {
        Self {
            creatures: creatures
                .iter()
                .map(|row| (row.asset.clone(), row))
                .collect(),
            live: HashMap::new(),
            own: HashMap::new(),
            facets: HashMap::new(),
            own_mount_interest: None,
            trace: AiTrace::default(),
        }
    }

    fn bundle(&mut self, context: u64, time_ms: u64, body: &[u8]) {
        let Ok(bundle) = ReplicatedStateBundle::decode(body) else {
            return;
        };
        self.trace.stats.bundles += 1;
        if let Some(block) = &bundle.index_block {
            let stops = usize::try_from(block.stop_count).unwrap_or(usize::MAX);
            for interest in block.indexes.iter().take(stops) {
                self.live.remove(&(context, *interest));
            }
        }
        for group in payload_groups(&bundle.payload, &mut self.trace.stats) {
            self.trace.stats.groups += 1;
            self.group(context, time_ms, &group);
        }
    }

    fn group(&mut self, context: u64, time_ms: u64, group: &ReadGroup) {
        self.own_mount(time_ms, group);
        if group.interest == LOCAL_PLAYER_INTEREST {
            self.own_states(context, time_ms, group);
            return;
        }
        let slot = (context, group.interest);
        let vitals_id = group.members.iter().find_map(|member| match &member.state {
            MemberState::Vitals(state) => state.slot_0.and_then(|slot| slot.vitals_id),
            _ => None,
        });
        let card = group.members.iter().find_map(|member| match &member.state {
            MemberState::GdeMetadata(card) => Some(card),
            _ => None,
        });
        let card_group = card.is_some() && vitals_id.is_some();
        if let (Some(card), Some(vitals_id)) = (card, vitals_id) {
            self.card(slot, time_ms, card, vitals_id);
        }
        let Some(mob) = self.live.get_mut(&slot) else {
            return;
        };
        let entry = &mut self.trace.mobs[mob.index];
        entry.last_ms = time_ms;
        let creature = self.creatures.get(&entry.asset).copied();
        let depleted = group.members.iter().any(|member| {
            matches!(&member.state, MemberState::GatherableController(state)
                if state.gatherable_state == Some(DEPLETED_GATHERABLE))
        });
        if depleted {
            entry.depleted_ms.get_or_insert(time_ms);
        }
        for member in &group.members {
            let MemberState::Alc(state) = &member.state else {
                continue;
            };
            if *mob.alc_component.get_or_insert(member.component) != member.component {
                self.trace.stats.other_component_states += 1;
                continue;
            }
            let Ok(children) = state.slot0_children() else {
                continue;
            };
            let sample = mob.alc.apply(
                time_ms,
                card_group,
                state,
                &children,
                creature,
                &mut self.trace.stats.bad_scope_blobs,
            );
            entry.samples.push(sample);
            self.trace.stats.mob_states += 1;
        }
    }

    /// Starts a mob lifetime at the card of a mob, a card with its vitals id; the card of the
    /// player slice ends the slot's mob.
    fn card(
        &mut self,
        slot: (u64, u16),
        time_ms: u64,
        card: &GdeMetadataReplicatedState,
        vitals_id: u32,
    ) {
        let (Some(asset), Some(gde_ref)) = (card.asset, card.gde_ref) else {
            return;
        };
        if self
            .live
            .get(&slot)
            .is_some_and(|mob| mob.gde_ref == gde_ref)
        {
            return;
        }
        self.live.remove(&slot);
        let asset_hex = hex::encode(asset.id);
        let creature = self.creatures.get(&asset_hex).copied();
        if asset == PLAYER_SLICE_ASSET {
            return;
        }
        let index = self.trace.mobs.len();
        self.trace.mobs.push(MobTrace {
            context: slot.0,
            interest: slot.1,
            asset: asset_hex,
            slice: creature.map(|row| row.slice.clone()),
            vitals_id,
            gde_ref: hex::encode(gde_ref),
            first_ms: time_ms,
            last_ms: time_ms,
            depleted_ms: None,
            samples: Vec::new(),
        });
        self.facets.insert(facet_of(&gde_ref), index);
        self.live.insert(
            slot,
            LiveMob {
                index,
                gde_ref,
                alc_component: None,
                alc: AlcTrack::default(),
            },
        );
    }

    /// The own player's mount states in `group`: the ride (`m_isMounted`) and the dash of what
    /// others see.
    fn own_mount(&mut self, time_ms: u64, group: &ReadGroup) {
        for member in &group.members {
            let MemberState::Mount(mount) = &member.state else {
                continue;
            };
            // The ride goes to the owner only: a state that sends it is the own one, and later
            // states of the same group's interest are the own rider's.
            if mount.mounted.is_some() {
                self.own_mount_interest = Some(group.interest);
            }
            if self.own_mount_interest != Some(group.interest) {
                continue;
            }
            let dashing = mount
                .remote_flags
                .map(|flags| flags.0 & RemoteFlags::DASHING != 0);
            if mount.mounted.is_some() || dashing.is_some() {
                self.trace.player_mount.push(MountSample {
                    time_ms,
                    mounted: mount.mounted,
                    dashing,
                });
            }
        }
    }

    /// The own player's ALC states in a group of interest 1.
    fn own_states(&mut self, context: u64, time_ms: u64, group: &ReadGroup) {
        let track = self.own.entry(context).or_default();
        for member in &group.members {
            let MemberState::Alc(state) = &member.state else {
                continue;
            };
            let Ok(children) = state.slot0_children() else {
                continue;
            };
            if member.component != OWN_ALC_COMPONENT {
                continue;
            }
            let sample = track.apply(
                time_ms,
                false,
                state,
                &children,
                None,
                &mut self.trace.stats.bad_scope_blobs,
            );
            if let Some(position) = sample.position {
                self.trace.player.push(PlayerSample { time_ms, position });
            }
        }
    }

    fn damage(&mut self, time_ms: u64, body: &[u8]) {
        let Ok(damage) = OnDamageMsg::decode(body) else {
            return;
        };
        let facet = damage.u64_c.to_be_bytes();
        let mob = self.facets.get(&facet).copied();
        if mob.is_none() {
            self.trace.stats.unknown_attackers += 1;
        }
        self.trace.player_hits.push(PlayerHit {
            time_ms,
            mob,
            attacker_facet: hex::encode(facet),
            attacker_at: damage.triple.map(f32::from_bits),
            amount: damage.items.iter().map(|item| item.amount).sum(),
        });
    }
}

impl AlcTrack {
    /// Applies one state (`card_state` when it came with the card), its slot-0 `children` and
    /// its `pos10`, and gives its sample.
    fn apply(
        &mut self,
        time_ms: u64,
        card_state: bool,
        state: &AlcState,
        children: &[(usize, Vec<u8>)],
        creature: Option<&CreatureRow>,
        bad_scope_blobs: &mut usize,
    ) -> MobSample {
        let mut sample = MobSample {
            time_ms,
            card_state,
            state_id: None,
            position: None,
            yaw: None,
            facing_gap: None,
            full_body: None,
        };
        let mut scope_blob = None;
        self.pos10 = state.pos10.or(self.pos10);
        for (child, bytes) in children {
            match *child {
                ID_REL_CHILD => self.id_rel = bytes.first().copied(),
                ID_ABS_CHILD => self.id_abs = bytes.first().copied(),
                POS_REL_CHILD => {
                    if let Ok(rel) = <[u8; AXES]>::try_from(bytes.as_slice()) {
                        self.rel = rel;
                    }
                }
                ROTATION_CHILD => sample.yaw = quat_yaw(bytes),
                SCOPE_DATA_CHILD => {
                    self.scope_mask = Reader::new(bytes).prefixed().ok();
                }
                SCOPE_INFO_CHILD => scope_blob = Some(bytes.as_slice()),
                FACING_GAP_CHILD => {
                    sample.facing_gap = <[u8; 4]>::try_from(bytes.as_slice())
                        .ok()
                        .map(f32::from_be_bytes);
                }
                _ => {}
            }
        }
        if let (Some(abs), Some(rel)) = (self.id_abs, self.id_rel) {
            sample.state_id = Some(u16::from_be_bytes([abs, rel]));
        }
        sample.position = self.pos10.map(|anchor| {
            let [dx, dy, dz] = self.rel.map(nw_messages::offset_metres);
            [anchor.x + dx, anchor.y + dy, anchor.z() + dz]
        });
        if let (Some(blob), Some(creature)) = (scope_blob, creature) {
            match self.full_body_entry(blob, creature) {
                Some(Some(entry)) if self.full_body != Some(entry) => {
                    self.full_body = Some(entry);
                    sample.full_body = Some(FullBodyEntry {
                        start_id: entry.start_id,
                        fragment: entry.fragment,
                        fragment_name: name_of(&creature.fragments, entry.fragment),
                        action: entry.action,
                        action_name: name_of(&creature.actions, entry.action),
                    });
                }
                Some(Some(_)) => {}
                Some(None) => self.full_body = None,
                None => *bad_scope_blobs += 1,
            }
        }
        sample
    }

    /// The FullBody entry of `blob`: `Some(None)` when no FullBody entry plays, `None` when the
    /// blob does not read or does not fit the scope mask. The entries are those of the set bits
    /// of the mask, in bit order. A blob of one entry whose count the mask does not explain is
    /// the FullBody entry (Inferred: after the card's spawn state with `scopeData` 0, retail
    /// leaves `scopeData` out; all 178 such entries of the newchar capture name a FullBody
    /// fragment: Move, Combat_Turn, Idle, POI_Graze).
    fn full_body_entry(&self, blob: &[u8], creature: &CreatureRow) -> Option<Option<ScopeEntry>> {
        let bit = creature
            .scopes
            .iter()
            .position(|scope| scope == FULL_BODY_SCOPE)?;
        let entries = scope_entries(blob)?;
        let mask = self.scope_mask.unwrap_or_default();
        if usize::try_from(mask.count_ones()).ok() != Some(entries.len()) {
            return match entries.as_slice() {
                [entry] => Some(Some(*entry)),
                _ => None,
            };
        }
        if mask >> bit & 1 == 0 {
            return Some(None);
        }
        let below = (mask & ((1 << bit) - 1)).count_ones();
        entries.get(usize::try_from(below).ok()?).copied().map(Some)
    }
}

/// The trace of the ledger `bytes`, with the slice and animation names of `creatures`. Ledger
/// records after a bad record are not read.
pub fn trace_ledger(bytes: &[u8], creatures: &[CreatureRow]) -> AiTrace {
    let mut tracer = Tracer::new(creatures);
    let mut decoder = Decoder::new();
    let mut first_ms = None;
    for record in ledger_records(bytes) {
        let Ok(record) = record else {
            break;
        };
        let time_ms = record.time_ms - *first_ms.get_or_insert(record.time_ms);
        let output = decoder.record(&record);
        if output.new_connection {
            tracer
                .live
                .retain(|(context, _), _| *context != record.context);
            tracer.own.remove(&record.context);
        }
        for delivered in &output.envelopes {
            let body = &delivered.envelope.body;
            let Some(type_index) = delivered.envelope.type_index() else {
                continue;
            };
            if record.direction != SERVER_TO_CLIENT {
                continue;
            }
            match type_index {
                REPLICATED_STATE_BUNDLE_TYPE_INDEX => {
                    tracer.bundle(record.context, time_ms, body);
                }
                LEVEL_INFO_CHANGED_MSG_TYPE_INDEX => {
                    tracer.live.clear();
                    tracer.own.clear();
                }
                ON_DAMAGE_TYPE_INDEX => tracer.damage(time_ms, body),
                _ => {}
            }
        }
    }
    tracer.trace
}

/// Reads the creature animation table file at `path`.
///
/// # Errors
///
/// Returns [`AiTraceError`] when the file cannot be read or is not the table.
pub fn read_creatures(path: &Path) -> Result<Vec<CreatureRow>, AiTraceError> {
    let text = std::fs::read_to_string(path).map_err(|source| AiTraceError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(serde_json::from_str(&text)?)
}

/// Writes `trace` as JSON at `path`, creating parent folders.
///
/// # Errors
///
/// Returns [`AiTraceError`] when the file cannot be written.
pub fn write_trace(path: &Path, trace: &AiTrace) -> Result<(), AiTraceError> {
    let io = |source| AiTraceError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(io)?;
    }
    let mut out = BufWriter::new(std::fs::File::create(path).map_err(io)?);
    serde_json::to_writer(&mut out, trace)?;
    out.flush().map_err(io)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nw_capture::test_support::{envelope, record_at, server_datagram_at};
    use nw_messages::alc::rotation_bytes;
    use nw_messages::{
        AlcStamp, AssetId, BundleGroup, FRAG_TAGS_LEN, GDE_METADATA_COMPONENT, MeleeDamage,
        MobAnimation, MobStep, StepPlace, StepScope, VitalsState,
    };
    use nw_reflect::{TypeId, encode_type_id};

    use super::*;

    const CLIENT_CONTEXT: u8 = 2;
    const MOB_INTEREST: u16 = 6;
    const LONE_CARD_INTEREST: u16 = 7;
    const ALC_COMPONENT: u64 = 5;
    const VITALS_COMPONENT: u64 = 9;
    const EXTRA_COMPONENT: u64 = 12;
    const UNREAD_COMPONENT: u64 = 13;
    /// A class with no reader here or in `extra_members`. 3451 is the group data state.
    const UNREAD_CLASS: u64 = 65_000;
    /// Body bytes of the unread member.
    const UNREAD_BODY: [u8; 4] = [0; 4];
    /// A loot drop body: slot 0, child 0, a `u32`.
    const LOOT_DROP_BODY: [u8; 6] = [0x01, 0x01, 0x00, 0x00, 0x00, 0x07];
    const MOB_NUMBER: u64 = 0x0102_0304_0506_0708;
    const PLAYER_NUMBER: u64 = 0x1112_1314_1516_1718;
    const MOB_ASSET: [u8; GDE_REF_LEN] = [0xe3; GDE_REF_LEN];
    const SUB_ID: u32 = 2;
    const MOB_VITALS_ID: u32 = 0x8869_82c9;
    const HEALTH: u32 = 100;
    const CATEGORY_ID: u32 = 1;
    const LEVEL: u32 = 3;
    const ANCHOR_US: u64 = 1_000_000;
    const FIRST_ID: u16 = 0x0102;
    const AT: [f32; AXES] = [484.0, 848.0, 91.0];
    const MOVED: [f32; AXES] = [485.0, 847.5, 91.0];
    const YAW: f32 = 1.2;
    const FACING_GAP: f32 = 0.5;
    const PLAYER_AT: [f32; AXES] = [490.0, 850.0, 92.0];
    const ATTACKER_AT: [f32; AXES] = [484.5, 848.5, 91.0];
    const DAMAGE: f32 = 13.0;
    const PLAYER_MAX_HEALTH: f32 = 200.0;
    const DAMAGE_TAG: u8 = 6;
    const ARMOR_SCALE: f32 = 0.6;
    /// The first record: the trace's times count from it.
    const START_MS: u64 = 0;
    const CARD_MS: u64 = 100;
    const MOVE_MS: u64 = 200;
    const RESYNC_MS: u64 = 250;
    const ATTACK_MS: u64 = 300;
    const SAME_MS: u64 = 350;
    const HIT_MS: u64 = 400;
    const STOP_MS: u64 = 500;
    const AFTER_STOP_MS: u64 = 600;
    /// Fragment and action numbers of the two animations.
    const IDLE: (u16, u16) = (2, 3);
    const ATTACK: (u16, u16) = (3, 4);
    /// The largest error of a `pos10` height and a `worldPosRel` step, in metres.
    const POSITION_TOLERANCE: f32 = nw_messages::POS_REL_STEP_M;
    /// The largest yaw error of a quat byte.
    const YAW_TOLERANCE: f32 = 0.01;
    const TEST_YAWS: [f32; 7] = [0.0, 0.3, -0.3, 1.5, -1.5, 3.0, -3.0];

    fn creatures() -> Vec<CreatureRow> {
        let names = |prefix: &str, count: usize| -> Vec<String> {
            (1..=count).map(|n| format!("{prefix}{n}")).collect()
        };
        vec![CreatureRow {
            asset: hex::encode(MOB_ASSET),
            slice: "slices/characters/test.dynamicslice".to_owned(),
            fragments: names("Fragment", 3),
            scopes: vec![FULL_BODY_SCOPE.to_owned(), "LookPose".to_owned()],
            actions: names("Action", 4),
            tags: BTreeMap::new(),
            default_tags: Vec::new(),
            fragment_ticks: BTreeMap::new(),
            collider_radius_mm: None,
            skinnable: false,
            weapon_animation_database: None,
        }]
    }

    fn position(at: [f32; AXES]) -> InputPosition {
        InputPosition::from_metres(at[0], at[1], at[2]).expect("finite")
    }

    fn animation((fragment, action): (u16, u16), start_id: u16) -> StepScope {
        StepScope::Changed(MobAnimation {
            entry: ScopeEntry {
                start_id,
                fragment,
                action,
                tail: 0,
            },
            ticks: 0,
            tags: [0; FRAG_TAGS_LEN],
            look_pose: None,
            facing_gap: Some(FACING_GAP),
        })
    }

    fn alc(id: u16, place: Option<StepPlace>, scope: Option<StepScope>) -> BundleMember {
        alc_of(ALC_COMPONENT, id, place, scope)
    }

    fn alc_of(
        component: u64,
        id: u16,
        place: Option<StepPlace>,
        scope: Option<StepScope>,
    ) -> BundleMember {
        let step = MobStep {
            stamp: AlcStamp {
                id,
                anchor_us: ANCHOR_US,
                offset_ms: 0,
            },
            anchored: false,
            position: place,
            facing: place.map(|_| YAW),
            scope,
            awake: false,
        };
        BundleMember {
            component,
            state: MemberState::Alc(AlcState::mob_step(&step).expect("step")),
        }
    }

    fn card(number: u64) -> BundleMember {
        BundleMember {
            component: GDE_METADATA_COMPONENT,
            state: MemberState::GdeMetadata(GdeMetadataReplicatedState::for_entity(
                AssetId {
                    id: MOB_ASSET,
                    sub_id: SUB_ID,
                },
                number,
            )),
        }
    }

    fn vitals() -> BundleMember {
        BundleMember {
            component: VITALS_COMPONENT,
            state: MemberState::Vitals(VitalsState::creature(
                HEALTH,
                HEALTH,
                MOB_VITALS_ID,
                CATEGORY_ID,
                LEVEL,
            )),
        }
    }

    /// A member of `class` with `body`, encoded by hand.
    fn raw_member(component: u64, class: u64, body: &[u8]) -> Vec<u8> {
        let mut writer = Writer::new();
        writer
            .prefixed(component)
            .expect("component")
            .bytes(&encode_type_id(&TypeId::FileSlot(class)).expect("class"))
            .bytes(body);
        writer.finish()
    }

    /// A group of encoded `members`.
    fn group(interest: u16, members: &[Vec<u8>]) -> Vec<u8> {
        let mut writer = Writer::new();
        writer
            .prefixed(u64::from(interest))
            .expect("interest")
            .u8(u8::try_from(members.len()).expect("few"));
        for member in members {
            writer.bytes(member);
        }
        writer.finish()
    }

    fn encoded(members: Vec<BundleMember>, interest: u16) -> Vec<u8> {
        BundlePayload {
            groups: vec![BundleGroup {
                interest: u64::from(interest),
                members,
            }],
        }
        .encode()
        .expect("payload")
    }

    struct Ledger {
        bytes: Vec<u8>,
        sequence: u16,
    }

    impl Ledger {
        fn server(&mut self, time_ms: u64, index: u64, body: Vec<u8>) {
            let datagram = server_datagram_at(self.sequence, &[envelope(index, body)]);
            self.sequence += 1;
            self.bytes
                .extend(record_at(time_ms, SERVER_TO_CLIENT, &datagram));
        }

        fn bundle(&mut self, time_ms: u64, stops: Vec<u16>, payload: Vec<u8>) {
            let empty = ReplicatedStateBundle::of_groups(1, CLIENT_CONTEXT, stops, Vec::new())
                .expect("bundle");
            let body = ReplicatedStateBundle { payload, ..empty }
                .encode()
                .expect("bundle");
            self.server(time_ms, REPLICATED_STATE_BUNDLE_TYPE_INDEX, body);
        }
    }

    fn ledger() -> Vec<u8> {
        let mut ledger = Ledger {
            bytes: Vec::new(),
            sequence: 1,
        };
        ledger.bundle(START_MS, Vec::new(), Vec::new());
        let first = alc(
            FIRST_ID,
            Some(StepPlace::Anchor(position(AT))),
            Some(animation(IDLE, FIRST_ID)),
        );
        let mut payload = encoded(vec![card(MOB_NUMBER), vitals(), first], MOB_INTEREST);
        payload.extend(encoded(vec![card(PLAYER_NUMBER)], LONE_CARD_INTEREST));
        let own = alc_of(
            OWN_ALC_COMPONENT,
            FIRST_ID,
            Some(StepPlace::Anchor(position(PLAYER_AT))),
            None,
        );
        payload.extend(encoded(
            vec![card(PLAYER_NUMBER), vitals(), own],
            LOCAL_PLAYER_INTEREST,
        ));
        ledger.bundle(CARD_MS, Vec::new(), payload);
        let moved = StepPlace::of(Some(position(AT)), position(MOVED));
        let step = alc(FIRST_ID + 1, Some(moved), None)
            .encode()
            .expect("member");
        let loot = raw_member(
            EXTRA_COMPONENT,
            extra_members::LOOT_DROP_TYPE_INDEX,
            &LOOT_DROP_BODY,
        );
        ledger.bundle(MOVE_MS, Vec::new(), group(MOB_INTEREST, &[loot, step]));
        let unread = raw_member(UNREAD_COMPONENT, UNREAD_CLASS, &UNREAD_BODY);
        let mut payload = group(LONE_CARD_INTEREST, &[unread]);
        payload.extend(encoded(vec![alc(FIRST_ID + 2, None, None)], MOB_INTEREST));
        ledger.bundle(RESYNC_MS, Vec::new(), payload);
        for (time_ms, id) in [(ATTACK_MS, FIRST_ID + 3), (SAME_MS, FIRST_ID + 4)] {
            let attack = alc(id, None, Some(animation(ATTACK, FIRST_ID + 3)));
            ledger.bundle(time_ms, Vec::new(), encoded(vec![attack], MOB_INTEREST));
        }
        let hit = OnDamageMsg::melee(
            PLAYER_NUMBER,
            &MeleeDamage {
                attacker: MOB_NUMBER,
                amount: DAMAGE,
                max_health: PLAYER_MAX_HEALTH,
                attacker_at: ATTACKER_AT,
                tag: DAMAGE_TAG,
                scale: ARMOR_SCALE,
                fire: None,
            },
        );
        ledger.server(HIT_MS, ON_DAMAGE_TYPE_INDEX, hit.encode().expect("hit"));
        ledger.bundle(STOP_MS, vec![MOB_INTEREST], Vec::new());
        ledger.bundle(
            AFTER_STOP_MS,
            Vec::new(),
            encoded(vec![alc(FIRST_ID + 5, None, None)], MOB_INTEREST),
        );
        ledger.bytes
    }

    fn assert_near(found: [f32; AXES], expected: [f32; AXES]) {
        for (found, expected) in found.iter().zip(expected) {
            assert!(
                (found - expected).abs() <= POSITION_TOLERANCE,
                "{found} vs {expected}"
            );
        }
    }

    #[test]
    fn a_mob_lifetime_gives_its_states_animations_and_hits() {
        let trace = trace_ledger(&ledger(), &creatures());
        let [mob] = trace.mobs.as_slice() else {
            panic!("one mob: {:?}", trace.mobs);
        };
        assert_eq!(
            (mob.interest, mob.vitals_id, mob.first_ms, mob.last_ms),
            (MOB_INTEREST, MOB_VITALS_ID, CARD_MS, SAME_MS)
        );
        assert_eq!(mob.slice.as_deref(), Some(creatures()[0].slice.as_str()));
        let times: Vec<u64> = mob.samples.iter().map(|sample| sample.time_ms).collect();
        assert_eq!(times, vec![CARD_MS, MOVE_MS, RESYNC_MS, ATTACK_MS, SAME_MS]);
        let ids: Vec<Option<u16>> = mob.samples.iter().map(|sample| sample.state_id).collect();
        assert_eq!(ids, (0..5).map(|n| Some(FIRST_ID + n)).collect::<Vec<_>>());
        let [first, moved, resynced, attack, same] = mob.samples.as_slice() else {
            panic!("five samples");
        };
        assert!(first.card_state && !moved.card_state);
        assert_near(first.position.expect("first place"), AT);
        assert_near(moved.position.expect("moved place"), MOVED);
        assert_eq!(resynced.position, moved.position);
        assert!((first.yaw.expect("yaw") - YAW).abs() < YAW_TOLERANCE);
        assert_eq!(first.facing_gap, Some(FACING_GAP));
        let idle = first.full_body.as_ref().expect("idle");
        assert_eq!(
            (idle.fragment_name.as_deref(), idle.action_name.as_deref()),
            (Some("Fragment2"), Some("Action3"))
        );
        let attack = attack.full_body.as_ref().expect("attack");
        assert_eq!(
            (attack.start_id, attack.fragment, attack.action),
            (FIRST_ID + 3, ATTACK.0, ATTACK.1)
        );
        assert_eq!(same.full_body, None);
        let [own] = trace.player.as_slice() else {
            panic!("one own position: {:?}", trace.player);
        };
        assert_eq!(own.time_ms, CARD_MS);
        assert_near(own.position, PLAYER_AT);
        let [hit] = trace.player_hits.as_slice() else {
            panic!("one hit: {:?}", trace.player_hits);
        };
        assert_eq!(
            (hit.time_ms, hit.mob, hit.attacker_at, hit.amount),
            (HIT_MS, Some(0), ATTACKER_AT, DAMAGE)
        );
        assert_eq!(trace.stats.unknown_attackers, 0);
        assert!(trace.stats.resyncs > 0);
    }

    #[test]
    fn quat_yaw_reads_every_rotation_the_server_writes() {
        for yaw in TEST_YAWS {
            let found = quat_yaw(&rotation_bytes(yaw)).expect("quat");
            let gap = (found - yaw).sin().abs();
            assert!(gap < YAW_TOLERANCE, "{yaw}: {found}");
        }
        assert_eq!(quat_yaw(&[]), None);
    }

    #[test]
    fn full_body_entry_follows_the_mask_and_takes_a_lone_entry() {
        const LOOK_POSE_ONLY: u64 = 0b10;
        const FULL_BODY_AND_LOOK_POSE: u64 = 0b11;
        let [creature] = creatures().try_into().expect("one row");
        let first = ScopeEntry {
            start_id: FIRST_ID,
            fragment: IDLE.0,
            action: IDLE.1,
            tail: 0,
        };
        let second = ScopeEntry {
            action: ATTACK.1,
            ..first
        };
        let blob = |entries: &[ScopeEntry]| {
            let mut body = Writer::new();
            for entry in entries {
                body.u16(entry.start_id)
                    .prefixed(u64::from(entry.fragment))
                    .expect("fragment")
                    .prefixed(u64::from(entry.action))
                    .expect("action")
                    .u16(0)
                    .u8(entry.tail);
            }
            let body = body.finish();
            let mut blob = Writer::new();
            blob.prefixed(body.len() as u64)
                .expect("count")
                .bytes(&body);
            blob.finish()
        };
        let with_mask = |mask| AlcTrack {
            scope_mask: mask,
            ..AlcTrack::default()
        };
        let both = blob(&[first, second]);
        let track = with_mask(Some(FULL_BODY_AND_LOOK_POSE));
        assert_eq!(track.full_body_entry(&both, &creature), Some(Some(first)));
        let track = with_mask(Some(LOOK_POSE_ONLY));
        assert_eq!(
            track.full_body_entry(&blob(&[second]), &creature),
            Some(None)
        );
        assert_eq!(track.full_body_entry(&both, &creature), None);
        let stale = with_mask(None);
        assert_eq!(
            stale.full_body_entry(&blob(&[first]), &creature),
            Some(Some(first))
        );
    }

    #[test]
    fn scope_entries_refuses_a_cut_blob() {
        let blob = [0x07, 0x00, 0x01, 0x02, 0x03];
        assert_eq!(scope_entries(&blob), None);
        assert_eq!(scope_entries(&[0x00]), Some(Vec::new()));
    }
}
