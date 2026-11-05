// SPDX-License-Identifier: AGPL-3.0-or-later
//! The loot bags of killed mobs (loot note 2026-09-18, retail kill at ledger
//! `newchar-retail-2026-09-18` 1792696951908, and 7,827 bags of 695 retail ledgers).
//!
//! Retail rolls the loot of every kill of a mob type with a loot table and drops a bag at the
//! corpse (a spawn group of its own: three delayed events, the card of asset `9844f2de...` sub 2
//! whose `GdeRef` is the bag id, the position). When the roll gave the killer items, the killer's
//! loot tracker (982) lists the bag; the client asks for its items (387) and gets them in its
//! global storage (2938); its interaction of kind 28 with the bag takes them (3743 named
//! `@inv_DroppedItems`, the items into the bag, the tracker's entry looted, the storage empty)
//! and the bag fades 1.25 s later and goes. A bag nobody may loot (an empty roll; 5,171 of the
//! 7,827 bags lived 6.0 to 6.5 s) starts its fade 4,967 ms after it spawned and goes 1.25 s
//! later. Only the killer rolls: how retail shares a group's loot is Unknown. The 7,827 bags are
//! the scans of 695 ledgers (`local-data/scratch-work/loot/bagtime`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::ItemState;
use nw_bus::LootLimitCount;
use nw_content::loot::LootRoll;
use nw_content::loot_limits::{LimitCount, LootLimitCounts, LootLimitRules};
use nw_content::perks::{chance_hit, rarity};
use nw_content::starter::KitItem;
use nw_messages::gde::AssetId;
use nw_messages::items::{ITEM_BOUND, SYNC_MOVED, SYNC_OWNED};
use nw_messages::loot::{
    GLOBAL_STORAGE_COMPONENT, GlobalStorageState, LOOT_CREATURE, LOOT_DROPPED, LOOT_POI,
    LOOT_QUEST_OBJECT, LOOT_ROLLED, LOOT_TAKEN, LOOT_TRACKER_COMPONENT, LootLimitEntry,
    LootTrackerState, StoredItems, TrackedLoot,
};
use nw_messages::position::{PositionInTheWorldReplicatedState, WorldPosition};
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    BundleGroup, BundleMember, DelayedEventState, FullList, GdeMetadataReplicatedState,
    InteractState, InteractionRequest, ItemRecord, MemberState,
};
use nw_sim_core::Position;
use uuid::Uuid;

use super::gathering::{GATHER_ACCEPTED, GATHER_BUSY};
use super::gear::{BagEntry, unix_ns};
use super::slots::Slot;
use super::statics::interactor_target;
use super::{Player, SimWorld, gather_answer, send_now};

/// First slot key of a loot bag: its serial is added. Keys are never sent; they stay above every
/// player and mob number and below the gather nodes.
pub(super) const BAG_ENTITY_BASE: u64 = 1 << 38;
/// A chance that always hits: a mob type without a `LootDropChance` rolls at every kill.
const SURE_CHANCE: f64 = 1.0;
/// Names the bag ids this server makes.
const BAG_ID_NAMESPACE: u64 = 0x6c6f_6f74_6261_6773;

/// The loot bag's slice asset (retail card at the kill).
const BAG_ASSET: AssetId = AssetId {
    id: [
        0x98, 0x44, 0xf2, 0xde, 0xc3, 0xb1, 0x5b, 0xd3, 0x98, 0xfa, 0x87, 0x68, 0xb5, 0x44, 0x59,
        0x06,
    ],
    sub_id: 2,
};
/// The components of a bag's group: its card, position, three delayed events and interact
/// state (retail spawn: c2, c3, c4, c0, c1; c5 at the take).
const CARD_COMPONENT: u64 = 0;
const POSITION_COMPONENT: u64 = 1;
const LIFE_EVENT_COMPONENT: u64 = 2;
const FADE_EVENT_COMPONENT: u64 = 3;
const SPARE_EVENT_COMPONENT: u64 = 4;
const INTERACT_COMPONENT: u64 = 5;
/// `totalDelayDuration` values as retail sends them (binary16 seconds): the life event's 180 s
/// and the fade's 1.25 s.
const LIFE_DELAY: u16 = 0x59a0;
const FADE_DELAY: u16 = 0x3d00;
const NO_DELAY: u16 = 0;
/// The fade's 1.25 s (retail: the fade completed 1,248 and 1,249 ms after it started).
const FADE_MS: u64 = 1_250;
/// When a bag nobody may loot starts to fade: retail 4,950 to 5,000 ms after its spawn for 4,640
/// of 5,908 such bags seen fading, most often 4,966 and 4,967.
const UNCLAIMED_FADE_AFTER_MS: u64 = 4_967;
/// The name the accept of a bag's take carries (retail 3743 at the kill's take).
const DROPPED_ITEMS_NAME: &[u8] = b"@inv_DroppedItems";
/// The name of a refusal (retail sends a name only with an accept).
const NO_NAME: &[u8] = &[];
/// When the second group of a take goes (retail: 66 ms after the first).
const TAKE_SETTLE_MS: u64 = 66;
/// The interactors a taken bag counts while its loot window opens (retail 1, then 0).
const ONE_INTERACTOR: u32 = 1;
const NO_INTERACTOR: u32 = 0;
/// How long the killer's loot rights last: `m_resetTime` less `m_lootedTime` at the kill (retail
/// 300 s in 781 of 782 tracked bags).
pub(super) const LOOT_RIGHTS_MS: u64 = 300_000;
/// How long a looted bag stays in the tracker: retail `m_resetTime` 2.9 to 3.0 s after the take's
/// bundle in 437 of 647 looted bags, the entry gone right after it (Inferred: 3 s from the
/// take).
pub(super) const LOOTED_KEEP_MS: u64 = 3_000;
/// The byte after `m_interactTag`: retail 0 for every bag whose items have no perks and no
/// `ForceRarity` (230 of 234 one-item bags); for gear it follows the perk count (1-2 perks 1,
/// 3 perks 2, 4 perks 3, 5 perks 4: the item rarity, [`nw_content::perks::rarity`]). A bag of
/// rolled items carries the highest rarity of them (best guess for bags of several items).
const NO_PERK_RARITY: u8 = 0;
/// `m_interactTag` of every retail creature bag.
const NO_INTERACT_TAG: u32 = 0;
/// The counter of a stored item (retail 0).
const STORED_COUNTER: u32 = 0;

/// Where a bag is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// It lies at the corpse.
    Lying,
    /// Its owner took the items: the loot window opens and the fade starts.
    Taken,
    /// The take settled.
    Settled,
    /// It fades without a take.
    Fading,
    /// It faded: it goes at the next tick.
    Faded,
}

/// One bag.
#[derive(Debug, Clone, PartialEq)]
struct LootBag {
    id: [u8; RAW_ID_LEN],
    position: Position,
    /// The player whose loot it holds; `None` when nobody may loot it.
    owner: Option<u64>,
    items: Vec<(String, u32)>,
    /// The own values of each entry of `items` (one item each: its rolls, or a dropped stack's
    /// values); `None`: an item of its data values.
    values: Vec<Option<ItemState>>,
    spawned_ms: u64,
    stage: Stage,
    /// When the current stage started.
    stage_ms: u64,
    /// When its fade ends, once it started.
    faded_at_ms: Option<u64>,
    /// Counts its stage changes, so each viewer gets each one once.
    version: u32,
}

impl LootBag {
    fn enter(&mut self, stage: Stage, now_ms: u64) {
        self.stage = stage;
        self.stage_ms = now_ms;
        self.version += 1;
        if matches!(stage, Stage::Taken | Stage::Fading) {
            self.faded_at_ms = Some(now_ms.saturating_add(FADE_MS));
        }
    }

    /// Its spawn members (retail order).
    fn spawn_members(&self) -> Vec<BundleMember> {
        let event = |component, total_delay| BundleMember {
            component,
            state: MemberState::DelayedEvent(DelayedEventState {
                total_delay: Some(total_delay),
                completed: Some(false),
            }),
        };
        vec![
            event(LIFE_EVENT_COMPONENT, LIFE_DELAY),
            event(FADE_EVENT_COMPONENT, NO_DELAY),
            event(SPARE_EVENT_COMPONENT, NO_DELAY),
            BundleMember {
                component: CARD_COMPONENT,
                state: MemberState::GdeMetadata(GdeMetadataReplicatedState {
                    asset: Some(BAG_ASSET),
                    gde_ref: Some(self.id),
                }),
            },
            BundleMember {
                component: POSITION_COMPONENT,
                state: MemberState::Position(PositionInTheWorldReplicatedState {
                    position: WorldPosition::from_metres(
                        self.position.x,
                        self.position.y,
                        self.position.z,
                    ),
                    ..PositionInTheWorldReplicatedState::default()
                }),
            },
        ]
    }

    /// The members of its current stage (retail bodies of the take and of the fades).
    fn stage_members(&self) -> Vec<BundleMember> {
        let event = |component, total_delay, completed| BundleMember {
            component,
            state: MemberState::DelayedEvent(DelayedEventState {
                total_delay: Some(total_delay),
                completed,
            }),
        };
        let interactors = |count| BundleMember {
            component: INTERACT_COMPONENT,
            state: MemberState::Interact(InteractState {
                enabled: None,
                has_interactors: Some(count),
            }),
        };
        match self.stage {
            Stage::Lying => Vec::new(),
            Stage::Taken => vec![
                event(LIFE_EVENT_COMPONENT, NO_DELAY, None),
                event(FADE_EVENT_COMPONENT, FADE_DELAY, None),
                interactors(ONE_INTERACTOR),
            ],
            Stage::Settled => vec![
                event(LIFE_EVENT_COMPONENT, LIFE_DELAY, None),
                interactors(NO_INTERACTOR),
            ],
            Stage::Fading => vec![event(FADE_EVENT_COMPONENT, FADE_DELAY, None)],
            Stage::Faded => vec![event(FADE_EVENT_COMPONENT, NO_DELAY, Some(true))],
        }
    }
}

/// The loot bags of one sim.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct LootBags {
    bags: BTreeMap<u64, LootBag>,
    next: u64,
}

/// A bag its owner may take: its id and items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Takeable {
    pub(super) id: [u8; RAW_ID_LEN],
    pub(super) items: Vec<(String, u32)>,
    /// The own values of each entry of `items`.
    pub(super) values: Vec<Option<ItemState>>,
}

/// One item stack taken from a bag: (item id, count) and its own values, when it has them.
pub(super) type TakenItem = ((String, u32), Option<ItemState>);

impl LootBags {
    /// Drops a bag of `items` for `owner` at `position` at `now_ms`, with the own `values` of
    /// each entry (`None`: the item's data values; a missing entry is `None`); a bag without
    /// items is nobody's. `sim` names the sim, so its bag ids differ from other sims'. Returns
    /// its id.
    pub(super) fn drop_bag(
        &mut self,
        sim: Uuid,
        position: Position,
        owner: u64,
        items: Vec<(String, u32)>,
        mut values: Vec<Option<ItemState>>,
        now_ms: u64,
    ) -> [u8; RAW_ID_LEN] {
        values.resize(items.len(), None);
        let serial = self.next;
        self.next += 1;
        // A dynamic spawn: retail gives dynamic entities an even first id byte (GDE id note).
        let id =
            nw_messages::as_dynamic_raw_id(*Uuid::new_v5(&sim, &serial.to_be_bytes()).as_bytes());
        self.bags.insert(
            BAG_ENTITY_BASE + serial,
            LootBag {
                id,
                position,
                owner: (!items.is_empty()).then_some(owner),
                items,
                values,
                spawned_ms: now_ms,
                stage: Stage::Lying,
                stage_ms: now_ms,
                faded_at_ms: None,
                version: 0,
            },
        );
        id
    }

    /// The bag `id` that `player` may take now; `None` for another bag.
    pub(super) fn takeable(&self, id: &[u8; RAW_ID_LEN], player: u64) -> Option<Takeable> {
        self.bags
            .values()
            .find(|bag| bag.id == *id && bag.owner == Some(player) && bag.stage == Stage::Lying)
            .map(|bag| Takeable {
                id: bag.id,
                items: bag.items.clone(),
                values: bag.values.clone(),
            })
    }

    /// Player `player` takes bag `id` at `now_ms`: it starts to fade. Returns its items with
    /// their own values, or `None` when it may not.
    pub(super) fn take(
        &mut self,
        id: &[u8; RAW_ID_LEN],
        player: u64,
        now_ms: u64,
    ) -> Option<Vec<TakenItem>> {
        let bag = self
            .bags
            .values_mut()
            .find(|bag| bag.id == *id && bag.owner == Some(player) && bag.stage == Stage::Lying)?;
        bag.enter(Stage::Taken, now_ms);
        let values = std::mem::take(&mut bag.values)
            .into_iter()
            .chain(std::iter::repeat(None));
        Some(
            std::mem::take(&mut bag.items)
                .into_iter()
                .zip(values)
                .collect(),
        )
    }

    /// Whether a bag of id `id` is here.
    pub(super) fn holds_bag(&self, id: &[u8; RAW_ID_LEN]) -> bool {
        self.bags.values().any(|bag| bag.id == *id)
    }

    /// Whether bag `id` lies with items still in it.
    pub(super) fn lying(&self, id: &[u8; RAW_ID_LEN]) -> bool {
        self.bags
            .values()
            .any(|bag| bag.id == *id && bag.stage == Stage::Lying && !bag.items.is_empty())
    }

    /// Player `player` takes `quantity` (0 or more than it: all) of item `index` of bag `id` at
    /// `now_ms`: the item id and the quantity taken, with the item's own values; the bag starts
    /// to fade when nothing is left in it. `None` when it may not.
    pub(super) fn take_part(
        &mut self,
        id: &[u8; RAW_ID_LEN],
        player: u64,
        index: usize,
        quantity: u32,
        now_ms: u64,
    ) -> Option<TakenItem> {
        let bag = self
            .bags
            .values_mut()
            .find(|bag| bag.id == *id && bag.owner == Some(player) && bag.stage == Stage::Lying)?;
        let values = bag.values.get(index).cloned().flatten();
        let (item_id, held) = bag.items.get_mut(index)?;
        let taken = if quantity == 0 || quantity >= *held {
            *held
        } else {
            quantity
        };
        *held -= taken;
        let item_id = item_id.clone();
        bag.values.resize(bag.items.len(), None);
        let kept: Vec<bool> = bag.items.iter().map(|(_, left)| *left > 0).collect();
        let mut keep = kept.iter();
        bag.values
            .retain(|_| keep.next().copied().unwrap_or_default());
        bag.items.retain(|(_, left)| *left > 0);
        if bag.items.is_empty() {
            bag.enter(Stage::Taken, now_ms);
        }
        Some(((item_id, taken), values))
    }

    /// The bags' stages move on at `now_ms`, and faded bags go.
    pub(super) fn tick(&mut self, now_ms: u64) {
        self.bags.retain(|_, bag| {
            let faded = bag.faded_at_ms.is_some_and(|at| now_ms >= at);
            match bag.stage {
                Stage::Lying => {
                    let fade_after = if bag.owner.is_some() {
                        LOOT_RIGHTS_MS
                    } else {
                        UNCLAIMED_FADE_AFTER_MS
                    };
                    if now_ms.saturating_sub(bag.spawned_ms) >= fade_after {
                        bag.enter(Stage::Fading, now_ms);
                    }
                }
                Stage::Taken if now_ms.saturating_sub(bag.stage_ms) >= TAKE_SETTLE_MS => {
                    bag.enter(Stage::Settled, now_ms);
                }
                Stage::Settled | Stage::Fading if faded => bag.enter(Stage::Faded, now_ms),
                Stage::Faded => return now_ms <= bag.stage_ms,
                Stage::Taken | Stage::Settled | Stage::Fading => {}
            }
            true
        });
    }

    /// The bags, and those that hold some player's loot.
    #[cfg(test)]
    pub(super) fn counts(&self) -> (usize, usize) {
        let owned = self.bags.values().filter(|bag| bag.owner.is_some()).count();
        (self.bags.len(), owned)
    }

    /// The slot keys of the bags shown to a player at `position`: those within `radius_m` (the
    /// interest radius).
    pub(super) fn numbers_near(
        &self,
        position: Position,
        radius_m: f32,
    ) -> impl Iterator<Item = u64> + '_ {
        self.bags
            .iter()
            .filter(move |(_, bag)| bag.position.horizontal_distance(position) <= radius_m)
            .map(|(number, _)| *number)
    }

    /// The group of the bag of slot key `number` in `slot` for a player who got its version
    /// `sent`: its spawn members when the slot is new, its stage's after a change, else none.
    /// Updates `sent`.
    pub(super) fn group(
        &self,
        number: u64,
        slot: Slot,
        sent: &mut HashMap<u64, u32>,
    ) -> Option<BundleGroup> {
        let bag = self.bags.get(&number)?;
        let members = if slot.new {
            bag.spawn_members()
        } else if sent.get(&number) != Some(&bag.version) {
            bag.stage_members()
        } else {
            return None;
        };
        sent.insert(number, bag.version);
        Some(BundleGroup {
            interest: u64::from(slot.interest),
            members,
        })
    }
}

/// The bags of one player's loot tracker and the replicated lists that name them, and the
/// counts of its loot limits, which the same component sends as `m_lootLimitDataMap` (retail:
/// with the bag at a kill whose drop counted, and with the login state).
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct PlayerLoot {
    tracked: Vec<TrackedLoot>,
    limits: LootLimitCounts,
}

/// The last byte of a loot limit entry (retail 0 in all 1,406 entries).
const LIMIT_ENTRY_LAST: u8 = 0;

/// When a player's client gets its loot limit counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LimitsSent {
    /// After a count changed.
    Changed,
    /// With the spawn of a login or a level change, when it has counts.
    Spawn,
}

impl PlayerLoot {
    /// The loot of a player that arrives with loot limit counts `counts` (a join or a handoff).
    pub(super) fn with_limits(counts: &[LootLimitCount]) -> Self {
        Self {
            tracked: Vec::new(),
            limits: LootLimitCounts::from_counts(counts.iter().map(|count| LimitCount {
                id: count.limit_id.clone(),
                count: count.count,
                counted_ns: count.counted_ns,
                ends_ns: count.ends_ns,
            })),
        }
    }

    /// Its loot limit counts, for a save or a handoff.
    pub(super) fn limit_counts(&self) -> Vec<LootLimitCount> {
        self.limits
            .counts()
            .into_iter()
            .map(|count| LootLimitCount {
                limit_id: count.id,
                count: count.count,
                counted_ns: count.counted_ns,
                ends_ns: count.ends_ns,
            })
            .collect()
    }

    /// The tracker member with version `version`.
    fn tracker(&self, version: u64) -> BundleMember {
        self.member(true, None, version)
    }

    /// The tracker member with version `version`: its bags when `loot`, and its loot limit
    /// counts under `rules` when given (both maps at the same version, retail at the kill).
    fn member(&self, loot: bool, rules: Option<&LootLimitRules>, version: u64) -> BundleMember {
        BundleMember {
            component: LOOT_TRACKER_COMPONENT,
            state: MemberState::LootTracker(LootTrackerState {
                loot: loot.then(|| FullList {
                    version: Some(version),
                    elements: self.tracked.clone(),
                }),
                limits: rules.map(|rules| FullList {
                    version: Some(version),
                    elements: self
                        .limits
                        .shown(rules)
                        .into_iter()
                        .map(|shown| LootLimitEntry {
                            key: shown.key,
                            first_ns: shown.first_ns,
                            ends_ns: shown.ends_ns,
                            // A count above the wire's u16 is Unknown (`nw_content::loot_limits`).
                            count: u16::try_from(shown.count).unwrap_or(u16::MAX),
                            last: LIMIT_ENTRY_LAST,
                        })
                        .collect(),
                }),
            }),
        }
    }

    /// The tracker member with only the loot limit counts at version `version`, when `sent`
    /// calls for it: after a change, or at a spawn with counts to show.
    fn limits_member(
        &mut self,
        rules: &LootLimitRules,
        sent: LimitsSent,
        version: u64,
    ) -> Option<BundleMember> {
        let changed = self.limits.take_changed();
        let due = match sent {
            LimitsSent::Changed => changed,
            LimitsSent::Spawn => !self.limits.shown(rules).is_empty(),
        };
        due.then(|| self.member(false, Some(rules), version))
    }

    /// The killer gets bag `bag`, rolled at server time `server_ms` for a mob spawned at
    /// `spawn` (retail `m_position`: the point of the mob's spawn record, Inferred). Its items have the rarity `rarity` ([`NO_PERK_RARITY`] for none). Returns the
    /// tracker member with version `version`, with the loot limit counts under `rules` when the
    /// roll changed them (retail at the kill).
    pub(super) fn track_rolled(
        &mut self,
        bag: [u8; RAW_ID_LEN],
        spawn: Position,
        server_ms: u64,
        rules: Option<&LootLimitRules>,
        rarity: u8,
        version: u64,
    ) -> BundleMember {
        self.track_kind(bag, LOOT_CREATURE, spawn, server_ms, rules, rarity, version)
    }

    /// The player dropped bag `bag` at `at` at server time `server_ms`: the tracker lists it as
    /// dropped items, with the loot rights of a kill (retail `item_actions`: type 5, reset 300 s
    /// after the drop). Returns the tracker member with version `version`.
    pub(super) fn track_dropped(
        &mut self,
        bag: [u8; RAW_ID_LEN],
        at: Position,
        server_ms: u64,
        version: u64,
    ) -> BundleMember {
        self.track_kind(
            bag,
            LOOT_DROPPED,
            at,
            server_ms,
            None,
            NO_PERK_RARITY,
            version,
        )
    }

    /// The player opened loot container `container` at `at` at server time `server_ms`: the
    /// tracker lists it as looted point-of-interest loot until server time `reset_ms`, and the
    /// player may not open it again before (craft2 report: retail state 2, type 0, the reset of
    /// [`nw_content::chest_lockouts`]; an open-world chest is `privileged`, the tutorial chest is
    /// not). Returns the tracker member with version `version`.
    pub(super) fn track_opened(
        &mut self,
        container: [u8; RAW_ID_LEN],
        at: Position,
        server_ms: u64,
        reset_ms: u64,
        privileged: bool,
        version: u64,
    ) -> BundleMember {
        self.track_looted(
            TrackedLoot {
                bag: container,
                state: LOOT_TAKEN,
                kind: LOOT_POI,
                reset_ns: unix_ns(reset_ms),
                looted_ns: unix_ns(server_ms),
                privileged,
                position: [at.x, at.y, at.z],
                interact_tag: NO_INTERACT_TAG,
                rarity: NO_PERK_RARITY,
            },
            version,
        )
    }

    /// The player broke quest object `object` (interact tag `tag`) from `at` at server time
    /// `server_ms`: the tracker lists it as looted until server time `reset_ms` (Observed: retail
    /// nodules, state 2, type 4, not privileged, the reset 33 s after, the place the player stood
    /// as its position). Returns the tracker member with version `version`.
    pub(super) fn track_broken(
        &mut self,
        object: [u8; RAW_ID_LEN],
        tag: u32,
        at: Position,
        server_ms: u64,
        reset_ms: u64,
        version: u64,
    ) -> BundleMember {
        self.track_looted(
            TrackedLoot {
                bag: object,
                state: LOOT_TAKEN,
                kind: LOOT_QUEST_OBJECT,
                reset_ns: unix_ns(reset_ms),
                looted_ns: unix_ns(server_ms),
                privileged: false,
                position: [at.x, at.y, at.z],
                interact_tag: tag,
                rarity: NO_PERK_RARITY,
            },
            version,
        )
    }

    /// Lists `entry` in place of any entry of the same bag; the tracker member with version
    /// `version`.
    fn track_looted(&mut self, entry: TrackedLoot, version: u64) -> BundleMember {
        self.tracked.retain(|loot| loot.bag != entry.bag);
        self.tracked.push(entry);
        self.tracker(version)
    }

    /// The loot type and interact tag of the tracker's entry of `bag`, when it lists one.
    #[cfg(test)]
    pub(super) fn opened_kind(&self, bag: &[u8; RAW_ID_LEN]) -> Option<(u32, u32)> {
        self.tracked
            .iter()
            .find(|loot| loot.bag == *bag)
            .map(|loot| (loot.kind, loot.interact_tag))
    }

    /// Whether the tracker holds loot container `container` as opened: the player may not
    /// open it until its entry's reset ([`Self::expire`]).
    pub(super) fn opened(&self, container: &[u8; RAW_ID_LEN]) -> bool {
        self.tracked
            .iter()
            .any(|loot| loot.bag == *container && loot.kind == LOOT_POI)
    }

    /// Lists bag `bag` of loot type `kind` at `at`, rolled at server time `server_ms`, with the
    /// item rarity `rarity`; the tracker member with version `version`, with the loot limit
    /// counts under `rules` when they changed.
    #[allow(clippy::too_many_arguments, reason = "the fields of one tracker entry")]
    fn track_kind(
        &mut self,
        bag: [u8; RAW_ID_LEN],
        kind: u32,
        spawn: Position,
        server_ms: u64,
        rules: Option<&LootLimitRules>,
        rarity: u8,
        version: u64,
    ) -> BundleMember {
        self.tracked.push(TrackedLoot {
            bag,
            state: LOOT_ROLLED,
            kind,
            reset_ns: unix_ns(server_ms.saturating_add(LOOT_RIGHTS_MS)),
            looted_ns: unix_ns(server_ms),
            privileged: true,
            position: [spawn.x, spawn.y, spawn.z],
            interact_tag: NO_INTERACT_TAG,
            rarity,
        });
        let changed = rules.filter(|_| self.limits.take_changed());
        self.member(true, changed, version)
    }

    /// Bag `bag` was taken at server time `server_ms`. Returns the tracker member with version
    /// `version`.
    pub(super) fn taken(
        &mut self,
        bag: &[u8; RAW_ID_LEN],
        server_ms: u64,
        version: u64,
    ) -> BundleMember {
        for loot in self.tracked.iter_mut().filter(|loot| loot.bag == *bag) {
            loot.state = LOOT_TAKEN;
            loot.reset_ns = unix_ns(server_ms.saturating_add(LOOTED_KEEP_MS));
        }
        self.tracker(version)
    }

    /// The entries whose reset time passed at server time `server_ms` leave the tracker: the
    /// tracker member with version `version`, or `None` when none left.
    pub(super) fn expire(&mut self, server_ms: u64, version: u64) -> Option<BundleMember> {
        let now_ns = unix_ns(server_ms);
        let before = self.tracked.len();
        self.tracked.retain(|loot| loot.reset_ns > now_ns);
        (self.tracked.len() != before).then(|| self.tracker(version))
    }

    /// The first bag of dropped items the tracker lists.
    #[cfg(test)]
    pub(super) fn dropped_bag(&self) -> Option<[u8; RAW_ID_LEN]> {
        self.tracked
            .iter()
            .find(|loot| loot.kind == LOOT_DROPPED)
            .map(|loot| loot.bag)
    }

    /// Whether the tracker lists a bag it has not taken.
    pub(super) fn holds(&self, bag: &[u8; RAW_ID_LEN]) -> bool {
        self.tracked
            .iter()
            .any(|loot| loot.bag == *bag && loot.state == LOOT_ROLLED)
    }
}

/// A listed item's own values as its record carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RolledRecord {
    /// Its gear score.
    pub(super) gear_score: u32,
    /// Its perks as wire slots.
    pub(super) perks: [u32; nw_messages::items::PERK_SLOTS],
    /// Its durability; `None`: full.
    pub(super) durability: Option<u32>,
}

/// The global storage member with version `version` that lists bag `bag`'s `items` (retail: one
/// record per item, sync reason 17, in no container, counter 0, with a new instance id).
/// `items` holds the master rows of the items, their quantities and their own values (gear
/// score, perks as wire slots and durability; `None`: the item's data values).
pub(super) fn storage_of(
    bag: [u8; RAW_ID_LEN],
    items: &[(&KitItem, u32, Option<RolledRecord>)],
    version: u64,
) -> BundleMember {
    let namespace = Uuid::from_bytes(bag);
    let records = (0_u64..)
        .zip(items)
        .map(|(index, (item, quantity, rolled))| ItemRecord {
            quantity: *quantity,
            sync_reason: SYNC_OWNED,
            container_slot: None,
            item_index: item.item_index,
            durability: rolled
                .and_then(|rolled| rolled.durability)
                .map_or(item.durability, |durability| {
                    durability.min(item.durability)
                }),
            max_durability: item.durability,
            counter: STORED_COUNTER,
            gear_score: rolled.map_or(item.gear_score, |rolled| rolled.gear_score),
            perks: rolled.map_or(item.perks, |rolled| rolled.perks),
            instance: *Uuid::new_v5(&namespace, &index.to_be_bytes()).as_bytes(),
            flags: if item.bound { ITEM_BOUND } else { 0 },
            ..ItemRecord::EMPTY_SLOT
        })
        .collect();
    storage_member(
        vec![StoredItems {
            storage: bag,
            items: records,
        }],
        version,
    )
}

/// The global storage member with version `version` that lists nothing (retail at the take).
pub(super) fn storage_emptied(version: u64) -> BundleMember {
    storage_member(Vec::new(), version)
}

/// The global storage member with version `version` that lists `storages`.
pub(super) fn storage_member(storages: Vec<StoredItems>, version: u64) -> BundleMember {
    BundleMember {
        component: GLOBAL_STORAGE_COMPONENT,
        state: MemberState::GlobalStorage(GlobalStorageState {
            storages: Some(FullList {
                version: Some(version),
                elements: storages,
            }),
        }),
    }
}

/// The namespace of the bag ids of the sim started at `generation` (Unix ms).
pub(super) fn bag_namespace(generation: u64) -> Uuid {
    Uuid::from_u64_pair(BAG_ID_NAMESPACE, generation)
}

impl SimWorld {
    /// The drops of roll `rolled` for player `number` that its loot limits let through at the
    /// server time, counted, after the roll's limit visits counted; all of them without limits
    /// or without the player.
    pub(super) fn limit_loot(&mut self, number: u64, rolled: LootRoll) -> Vec<(String, u32)> {
        let content = Arc::clone(&self.content);
        let now_ns = unix_ns(self.server_ms);
        match (
            content.loot_limits.as_ref(),
            self.player_by_number(number)
                .and_then(|entity| self.world.get_mut::<Player>(entity)),
        ) {
            (Some(rules), Some(mut player)) => {
                for limit in &rolled.limit_visits {
                    player.loot.limits.count_one(rules, limit, now_ns);
                }
                player.loot.limits.admit(rules, rolled.items, now_ns)
            }
            _ => rolled.items,
        }
    }

    /// The firings of `times` that loot limit `limit` of player `number` lets through at the
    /// server time, each counted on it; its client gets the new counts. All of them without
    /// limits or without the player.
    pub(super) fn limited_firings(&mut self, number: u64, limit: &str, times: u32) -> u32 {
        let content = Arc::clone(&self.content);
        let now_ns = unix_ns(self.server_ms);
        let Some(rules) = content.loot_limits.as_ref() else {
            return times;
        };
        let Some(mut player) = self
            .player_by_number(number)
            .and_then(|entity| self.world.get_mut::<Player>(entity))
        else {
            return times;
        };
        let passed = (0..times)
            .filter(|_| player.loot.limits.count_one(rules, limit, now_ns))
            .count();
        self.send_loot_limits(number, LimitsSent::Changed);
        u32::try_from(passed).unwrap_or(times)
    }

    /// The lowercase ids of player `number`'s loot limits at their limit at the server time:
    /// its rolls skip their `[LIM]` tables. None without limits or without the player.
    pub(super) fn reached_loot_limits(&mut self, number: u64) -> Vec<String> {
        let now_ns = unix_ns(self.server_ms);
        let Some(entity) = self.player_by_number(number) else {
            return Vec::new();
        };
        self.content
            .loot_limits
            .as_ref()
            .zip(self.world.get::<Player>(entity))
            .map(|(rules, player)| player.loot.limits.reached(rules, now_ns))
            .unwrap_or_default()
    }

    /// Player `number`'s client gets its loot limit counts as `sent` calls for (see
    /// [`LimitsSent`]): a tracker member with them goes with its next bundle.
    pub(super) fn send_loot_limits(&mut self, number: u64, sent: LimitsSent) {
        let content = Arc::clone(&self.content);
        let Some(rules) = content.loot_limits.as_ref() else {
            return;
        };
        let Some(mut player) = self
            .player_by_number(number)
            .and_then(|entity| self.world.get_mut::<Player>(entity))
        else {
            return;
        };
        let version = player.list_version + 1;
        if let Some(member) = player.loot.limits_member(rules, sent, version) {
            player.list_version = version;
            player.extra_members.push(member);
        }
    }

    /// Mob `mob`, killed by player `killer` at `now_ms`, drops its loot bag at its corpse: the
    /// roll of its loot table at its type's `LootDropChance` ([`nw_content::loot::MobLoot`];
    /// every kill of a mob type with a table drops a bag, loot note) for the killer, less the
    /// items its loot limits hold back, which its loot tracker lists when the roll gave items.
    /// A killer on another sim, a mob type without a table and the tutorial drop nothing.
    pub(super) fn drop_loot(&mut self, mob: u64, killer: u64, now_ms: u64) {
        if self.is_tutorial() || self.player_by_number(killer).is_none() {
            return;
        }
        let content = Arc::clone(&self.content);
        let Some(drop) = self
            .mobs
            .vitals_of(mob)
            .and_then(|vitals| content.mob_loot.of(vitals))
        else {
            return;
        };
        let Some((corpse, spawn)) = self.mobs.places(mob) else {
            return;
        };
        let reached = self.reached_loot_limits(killer);
        // A mutated instance adds its loot tags and its injected creature tables.
        let tags = self.mutation_loot_tags(&drop.tags);
        let context = self.loot_context(killer, Some((mob, &tags)), &reached);
        let dice = &mut self.dice;
        let mut below = |below| dice.below(below);
        // A type without a chance rolls at every kill: a sure chance before the multiplier.
        let chance = (drop.chance.unwrap_or(SURE_CHANCE) * self.combat.loot_drop_multiplier)
            .clamp(0.0, SURE_CHANCE);
        let rolls_table = chance >= SURE_CHANCE || chance_hit(chance, &mut below);
        let rolled = if rolls_table {
            content.loot.roll(&drop.table, context, &mut below)
        } else {
            LootRoll::default()
        };
        let hints = rolled.gear_scores.clone();
        let mut items = self.limit_loot(killer, rolled);
        items.extend(self.mutation_kill_loot(killer, mob));
        let rolled = !items.is_empty();
        let (items, values) = self.roll_drops(killer, Some(mob), items, &hints);
        let rarity = values
            .iter()
            .flatten()
            .map(|item| rarity(item.perks.as_ref().map_or(0, Vec::len)))
            .max()
            .and_then(|rarity| u8::try_from(rarity).ok())
            .unwrap_or(NO_PERK_RARITY);
        let namespace = bag_namespace(self.me.generation);
        let bag = self
            .bags
            .drop_bag(namespace, corpse, killer, items, values, now_ms);
        let server_ms = self.server_ms;
        let Some(mut player) = self
            .player_by_number(killer)
            .and_then(|entity| self.world.get_mut::<Player>(entity))
        else {
            return;
        };
        if rolled {
            player.list_version += 1;
            let version = player.list_version;
            let tracked = player.loot.track_rolled(
                bag,
                spawn,
                server_ms,
                content.loot_limits.as_ref(),
                rarity,
                version,
            );
            player.extra_members.push(tracked);
        }
        self.send_loot_limits(killer, LimitsSent::Changed);
    }

    /// The client of player `entity` asks for the items of storage `storage` (387): a bag of
    /// its loot tracker gets its items into the player's global storage. Another storage is not
    /// known here.
    pub(super) fn show_bag_items(&mut self, entity: Entity, storage: &[u8; RAW_ID_LEN]) {
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return;
        };
        let Some(bag) = self.bags.takeable(storage, number) else {
            tracing::debug!(
                player = number,
                "items of a storage that is not a bag of its own"
            );
            return;
        };
        let content = Arc::clone(&self.content);
        let values = bag.values.iter().chain(std::iter::repeat(&None));
        let items: Vec<(&KitItem, u32, Option<RolledRecord>)> = bag
            .items
            .iter()
            .zip(values)
            .filter_map(|((item, quantity), values)| {
                let kit = content.kits.item(item)?;
                let rolled = values.as_ref().map(|values| RolledRecord {
                    gear_score: values.gear_score.unwrap_or(kit.gear_score),
                    perks: values
                        .perks
                        .as_ref()
                        .map_or(kit.perks, |perks| content.kits.perk_slots(perks)),
                    durability: values.durability,
                });
                Some((kit, *quantity, rolled))
            })
            .collect();
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.list_version += 1;
            let version = player.list_version;
            player
                .extra_members
                .push(storage_of(bag.id, &items, version));
        }
    }

    /// Player `number` (`entity`) uses loot bag `request` at `now_ms` (kind 28): its own bag
    /// that it did not take is taken (3743 named `@inv_DroppedItems`, the bag as its interaction
    /// target, the tracker's entry looted, the storage emptied, the items into its bag); another
    /// is refused (status 2, Inferred as for a busy gather), and so is its own while its bag has
    /// no room for the items (the loot stays in the bag; Inferred: retail's answer to a full
    /// bag is Unknown).
    pub(super) fn take_loot_bag(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
        now_ms: u64,
    ) {
        let fits = self
            .bags
            .takeable(&request.target, number)
            .is_none_or(|bag| {
                let content = Arc::clone(&self.content);
                let entries: Vec<BagEntry<'_>> = bag
                    .items
                    .iter()
                    .zip(bag.values.iter().chain(std::iter::repeat(&None)))
                    .filter_map(|((item, quantity), values)| {
                        Some((content.kits.item(item)?, *quantity, values.is_some()))
                    })
                    .collect();
                self.world
                    .get::<Player>(entity)
                    .and_then(|player| player.gear.as_ref())
                    .is_some_and(|gear| gear.has_room_for(&entries))
            });
        let items = fits
            .then(|| self.bags.take(&request.target, number, now_ms))
            .flatten();
        let server_ms = self.server_ms;
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let Some(items) = items.filter(|_| player.loot.holds(&request.target)) else {
            tracing::debug!(player = number, ?request, "bag refused");
            let refusal = gather_answer(number, request, GATHER_BUSY, NO_NAME);
            send_now(&mut self.combat_messages, &player.connection, &[refusal]);
            return;
        };
        let accept = gather_answer(number, request, GATHER_ACCEPTED, DROPPED_ITEMS_NAME);
        send_now(&mut self.combat_messages, &player.connection, &[accept]);
        player.list_version += 1;
        let version = player.list_version;
        let taken = player.loot.taken(&request.target, server_ms, version);
        player.extra_members.extend([
            interactor_target(request.target_entity),
            taken,
            storage_emptied(version),
        ]);
        self.give_drops(number, items, SYNC_MOVED, now_ms);
    }

    /// The loot tracker entries whose reset time came at server time `server_ms` leave their
    /// players' trackers.
    pub(super) fn loot_rights_due(&mut self, server_ms: u64) {
        let mut query = self.world.query::<&mut Player>();
        for mut player in query.iter_mut(&mut self.world) {
            let version = player.list_version + 1;
            if let Some(tracker) = player.loot.expire(server_ms, version) {
                player.list_version = version;
                player.extra_members.push(tracker);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::BundlePayload;

    use super::*;

    /// The interest radius of the test views.
    const RADIUS_M: f32 = 100.0;
    const AT: Position = Position {
        x: 9880.871,
        y: 2274.392,
        z: 42.9,
    };
    const KILLER: u64 = 7;
    const NOW: u64 = 1_000_000;

    fn bags_with(items: Vec<(String, u32)>) -> (LootBags, [u8; RAW_ID_LEN]) {
        let mut bags = LootBags::default();
        let id = bags.drop_bag(bag_namespace(1), AT, KILLER, items, Vec::new(), NOW);
        (bags, id)
    }

    fn pork() -> Vec<(String, u32)> {
        vec![("PorkT1".to_owned(), 2)]
    }

    fn encoded(group: BundleGroup) -> Vec<u8> {
        BundlePayload {
            groups: vec![group],
        }
        .encode()
        .expect("encode")
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    const NEW: Slot = Slot {
        interest: 52,
        new: true,
    };
    const KEPT: Slot = Slot {
        interest: 52,
        new: false,
    };

    #[test]
    fn a_new_bag_spawns_with_the_retail_members() {
        let (bags, id) = bags_with(pork());
        let number = bags.numbers_near(AT, RADIUS_M).next().expect("shown");
        let mut sent = HashMap::new();
        let group = bags.group(number, NEW, &mut sent).expect("spawn");
        let bytes = hex(&encoded(group));
        // Retail bag group of the kill (+57 ms): interest 52, five members, then the position
        // with this server's height and no rotation.
        let retail = format!(
            "3405028331010359a00003833101030000000483310103000000000a01039844f2dec3b15bd398fa\
             8768b544590600000002{}010d01",
            hex(&id)
        );
        assert!(bytes.starts_with(&retail), "{bytes}");
        assert_eq!(bags.group(number, KEPT, &mut sent), None, "sent once");
    }

    #[test]
    fn a_bag_id_has_the_dynamic_form() {
        let (_, id) = bags_with(pork());
        assert!(
            nw_messages::is_dynamic_raw_id(&id),
            "an even first byte: {}",
            hex(&id)
        );
    }

    #[test]
    fn a_bag_keeps_the_values_of_its_items_through_takes() {
        const SWORD: &str = "1hSwordT5";
        let sword = ItemState {
            gear_score: Some(610),
            durability: Some(12),
            perks: Some(vec!["PerkID_A".to_owned()]),
            ..ItemState::default()
        };
        let mut bags = LootBags::default();
        let items = vec![
            (SWORD.to_owned(), 1),
            ("PorkT1".to_owned(), 2),
            (SWORD.to_owned(), 1),
        ];
        let rolls = vec![Some(sword.clone()), None, Some(sword.clone())];
        let id = bags.drop_bag(bag_namespace(1), AT, KILLER, items, rolls, NOW);
        assert_eq!(
            bags.take_part(&id, KILLER, 0, 0, NOW),
            Some(((SWORD.to_owned(), 1), Some(sword.clone())))
        );
        let left = bags.takeable(&id, KILLER).expect("left");
        assert_eq!(left.items.len(), 2);
        assert_eq!(left.values, [None, Some(sword.clone())]);
        let taken = bags.take(&id, KILLER, NOW).expect("taken");
        assert_eq!(
            taken,
            [
                (("PorkT1".to_owned(), 2), None),
                ((SWORD.to_owned(), 1), Some(sword))
            ]
        );
        assert_eq!(bags.take(&id, KILLER, NOW), None, "taken once");
        let (mut plain, plain_id) = bags_with(pork());
        assert_eq!(
            plain.take(&plain_id, KILLER, NOW),
            Some(vec![(("PorkT1".to_owned(), 2), None)])
        );
    }

    #[test]
    fn a_taken_bag_sends_the_retail_take_groups_then_goes() {
        let (mut bags, id) = bags_with(pork());
        let number = BAG_ENTITY_BASE;
        let mut sent = HashMap::new();
        bags.group(number, NEW, &mut sent);
        assert_eq!(bags.take(&id, KILLER + 1, NOW), None, "not the owner's");
        assert_eq!(
            bags.take(&id, KILLER, NOW),
            Some(pork().into_iter().map(|item| (item, None)).collect())
        );
        assert_eq!(bags.take(&id, KILLER, NOW), None, "taken once");
        let taken = bags.group(number, KEPT, &mut sent).expect("take");
        // Retail group 52 at the take (+4291 ms): c2 0 s, c3 1.25 s, c5 one interactor.
        assert_eq!(
            hex(&encoded(taken)),
            "34030283310101000003833101013d0005b22d010200000001"
        );
        bags.tick(NOW + TAKE_SETTLE_MS - 1);
        assert_eq!(bags.group(number, KEPT, &mut sent), None);
        bags.tick(NOW + TAKE_SETTLE_MS);
        let settled = bags.group(number, KEPT, &mut sent).expect("settle");
        // Retail +4357 ms: c2 180 s, c5 no interactor.
        assert_eq!(
            hex(&encoded(settled)),
            "3402028331010159a005b22d010200000000"
        );
        bags.tick(NOW + FADE_MS - 1);
        assert_eq!(bags.group(number, KEPT, &mut sent), None);
        bags.tick(NOW + FADE_MS);
        let faded = bags.group(number, KEPT, &mut sent).expect("faded");
        // Retail +5540 ms: c3 completed.
        assert_eq!(hex(&encoded(faded)), "34010383310103000001");
        assert_eq!(
            bags.numbers_near(AT, RADIUS_M).count(),
            1,
            "it goes at the next tick"
        );
        bags.tick(NOW + FADE_MS + 1);
        assert_eq!(bags.numbers_near(AT, RADIUS_M).count(), 0);
    }

    #[test]
    fn an_empty_bag_fades_after_retails_delay() {
        let (mut bags, id) = bags_with(Vec::new());
        assert_eq!(bags.takeable(&id, KILLER), None, "nobody's");
        let number = BAG_ENTITY_BASE;
        let mut sent = HashMap::new();
        bags.group(number, NEW, &mut sent);
        bags.tick(NOW + UNCLAIMED_FADE_AFTER_MS - 1);
        assert_eq!(bags.group(number, KEPT, &mut sent), None);
        bags.tick(NOW + UNCLAIMED_FADE_AFTER_MS);
        let fading = bags.group(number, KEPT, &mut sent).expect("fade");
        // Retail unclaimed bag (+4967 ms): c3 1.25 s.
        assert_eq!(hex(&encoded(fading)), "340103833101013d00");
        bags.tick(NOW + UNCLAIMED_FADE_AFTER_MS + FADE_MS);
        assert!(bags.group(number, KEPT, &mut sent).is_some(), "completed");
        bags.tick(NOW + UNCLAIMED_FADE_AFTER_MS + FADE_MS + 1);
        assert_eq!(bags.numbers_near(AT, RADIUS_M).count(), 0);
    }

    #[test]
    fn an_untaken_bag_fades_when_the_loot_rights_end() {
        let (mut bags, id) = bags_with(pork());
        bags.tick(NOW + LOOT_RIGHTS_MS - 1);
        assert!(bags.takeable(&id, KILLER).is_some());
        bags.tick(NOW + LOOT_RIGHTS_MS);
        assert_eq!(bags.takeable(&id, KILLER), None);
    }

    #[test]
    fn a_far_bag_is_not_shown() {
        let (bags, _) = bags_with(pork());
        let far = Position {
            x: AT.x + RADIUS_M + 1.0,
            ..AT
        };
        assert_eq!(bags.numbers_near(far, RADIUS_M).count(), 0);
    }

    #[test]
    fn the_tracker_lists_takes_and_drops_the_bag() {
        let (_, id) = bags_with(pork());
        let mut loot = PlayerLoot::default();
        let spawn = Position {
            x: 9858.286,
            y: 2280.0605,
            z: 42.92239,
        };
        let tracked = loot.track_rolled(id, spawn, NOW, None, NO_PERK_RARITY, 1);
        let MemberState::LootTracker(state) = &tracked.state else {
            panic!("tracker");
        };
        let entry = state.loot.as_ref().expect("map").elements[0];
        assert_eq!(entry.state, LOOT_ROLLED);
        assert_eq!(
            entry.reset_ns - entry.looted_ns,
            LOOT_RIGHTS_MS * nw_messages::NS_PER_MS
        );
        assert!(loot.holds(&id));
        loot.taken(&id, NOW, 2);
        assert!(!loot.holds(&id));
        assert_eq!(loot.expire(NOW + LOOTED_KEEP_MS - 1, 3), None);
        let gone = loot.expire(NOW + LOOTED_KEEP_MS, 3).expect("expired");
        let MemberState::LootTracker(state) = gone.state else {
            panic!("tracker");
        };
        assert!(state.loot.expect("map").elements.is_empty());
    }

    #[test]
    fn an_opened_chest_stays_locked_for_its_opener_until_its_reset() {
        const CHEST: [u8; RAW_ID_LEN] = [0x3c; RAW_ID_LEN];
        const RESET_MS: u64 = NOW + 3_600_000;
        let mut loot = PlayerLoot::default();
        assert!(!loot.opened(&CHEST));
        loot.track_opened(CHEST, AT, NOW, RESET_MS, true, 1);
        assert!(loot.opened(&CHEST));
        assert!(!loot.holds(&CHEST), "looted, not waiting for a take");
        assert_eq!(loot.expire(RESET_MS - 1, 2), None);
        assert!(loot.expire(RESET_MS, 2).is_some());
        assert!(!loot.opened(&CHEST), "the reset frees it");
    }
}
