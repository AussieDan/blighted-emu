// SPDX-License-Identifier: AGPL-3.0-or-later
//! A player's items: what it wears and what is in its bag, sent as the gear states of its own
//! player. A login wears the kit of its archetype pick (starter-kit note); the tutorial then
//! takes the sword, fills the bag and equips from it (live-tutorial note, section 5). Outside the
//! tutorial the items are saved (admin studio S5), and a login starts from the saved items.

use std::collections::BTreeMap;

use nw_bus::{CarriedItem, ItemPlace};
use nw_content::starter::{EquipSlot, KitItem, StarterKits};

use crate::config::StartTool;
use nw_messages::items::SYNC_MOVED;
use nw_messages::paperdoll::slot;
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    EquippedItem, GearItem, Hands, ItemLook, LoadedAmmo, NS_PER_MS, PlayerGear, STARTER_BAG_SLOTS,
    is_visible_slot,
};
use uuid::Uuid;

mod charms;
mod durability;
mod holdings;
mod look;
mod rolls;

pub(super) use charms::{CharmRefusal, CharmTarget};
pub(super) use durability::RepairPick;
use holdings::Holdings;
pub(super) use holdings::{ItemRefusal, MAX_CONTAINER_SLOTS, MovedStack};
pub(super) use rolls::BagEntry;

/// The version of the first gear states (retail value).
const FIRST_GEAR_VERSION: u64 = 3;
/// How long a drawn weapon takes to reach the hand (retail 331 to 418 ms after the key).
const ATTACH_MS: u64 = 331;
/// The main-hand options whose ammo the paperdoll carries (owner children 8 and 9).
const AMMO_OPTIONS: usize = 2;
/// The largest stack quantity: the world database stores it as an `INTEGER`.
pub(crate) const MAX_STACK_QUANTITY: u32 = i32::MAX as u32;
/// The main-hand option selected at login (retail first paperdoll: active map slots 24 and 27).
const FIRST_ACTIVE_OPTION: u32 = slot::MAIN_HAND_OPTION_1;

/// The ns since 1970-01-01 at server time `server_ms` (ms since
/// [`crate::server::SERVER_EPOCH_UNIX_MS`]): retail sends Unix time in ns for a loaded-ammo state
/// (client-gates note), a loot right and a status effect's end.
pub(super) const fn unix_ns(server_ms: u64) -> u64 {
    server_ms
        .saturating_add(crate::server::SERVER_EPOCH_UNIX_MS)
        .saturating_mul(NS_PER_MS)
}

/// The server time of the ns since 1970-01-01 `unix_ns` ([`unix_ns`] back); 0 before the server
/// epoch.
pub(super) const fn server_ms_of(unix_ns: u64) -> u64 {
    (unix_ns / NS_PER_MS).saturating_sub(crate::server::SERVER_EPOCH_UNIX_MS)
}

/// The ammo of a main-hand option slot.
fn ammo_index(slot_id: u32) -> Option<usize> {
    let index = usize::try_from(slot_id.checked_sub(slot::MAIN_HAND_OPTION_1)?).ok()?;
    (index < AMMO_OPTIONS).then_some(index)
}

/// The other one of main-hand options 1 and 2, the options with ammo.
fn paired_option(slot_id: u32) -> Option<u32> {
    match ammo_index(slot_id)? {
        0 => Some(slot::MAIN_HAND_OPTION_1 + 1),
        _ => Some(slot::MAIN_HAND_OPTION_1),
    }
}

/// The shots of one main-hand option.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Ammo {
    /// The shots its weapon holds (0 for a melee weapon).
    max: u8,
    loaded: u8,
    /// How long its weapon takes to load again.
    reload_ms: u64,
    /// When the empty weapon is loaded again.
    reload_at_ms: Option<u64>,
    /// When the option was last put away (ns since 1970-01-01).
    time_ns: u64,
}

/// The paperdoll slots an item of `kind` may go into; an item that is not worn goes into a
/// slot that does not show (amulet, ring, quick slots).
fn fits(kind: Option<EquipSlot>, slot_id: u32) -> bool {
    match kind {
        Some(EquipSlot::Head) => slot_id == slot::HEAD,
        Some(EquipSlot::Chest) => slot_id == slot::CHEST,
        Some(EquipSlot::Hands) => slot_id == slot::HANDS,
        Some(EquipSlot::Legs) => slot_id == slot::LEGS,
        Some(EquipSlot::Feet) => slot_id == slot::FEET,
        Some(EquipSlot::MainHand) => {
            (slot::MAIN_HAND_OPTION_1..=slot::MAIN_HAND_OPTION_3).contains(&slot_id)
        }
        Some(EquipSlot::OffHand) => {
            (slot::OFF_HAND_OPTION_1..=slot::OFF_HAND_OPTION_3).contains(&slot_id)
        }
        None => !is_visible_slot(slot_id),
    }
}

/// The first worn slot of `kind`.
const fn first_slot(kind: EquipSlot) -> u32 {
    match kind {
        EquipSlot::Head => slot::HEAD,
        EquipSlot::Chest => slot::CHEST,
        EquipSlot::Hands => slot::HANDS,
        EquipSlot::Legs => slot::LEGS,
        EquipSlot::Feet => slot::FEET,
        EquipSlot::MainHand => slot::MAIN_HAND_OPTION_1,
        EquipSlot::OffHand => slot::OFF_HAND_OPTION_1,
    }
}

/// One item stack of a player.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stack {
    item_id: String,
    kind: Option<EquipSlot>,
    item: GearItem,
    /// Its owned-items key while it is worn or a saved loadout names it ([`EquippedItem::key`]).
    key: Option<u16>,
    /// The most one stack of its item holds; `None` for no limit of the item's own.
    max_stack: Option<u32>,
    /// Its perk ids in slot order, as it keeps them over saves and handoffs (`item.perks` holds
    /// their wire indexes).
    perks: Vec<String>,
    /// The sync reason of its bag record: the reason of its last change in the bag (retail
    /// marks a craft's stacks, [`nw_messages::items::SYNC_CRAFTED`]).
    reason: u8,
}

/// Why an equip request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EquipRefusal {
    /// The bag slot is empty or holds another item.
    NoSuchItem,
    /// The item does not go into that paperdoll slot.
    WrongSlot,
    /// The worn item has no free bag slot to go to.
    BagFull,
    /// A slot's item is not the one the request names.
    NotWorn,
    /// The item's class is disabled (`javelin.disabled-item-classes`).
    Disabled,
}

/// Why the server put the drawn weapon away while the player does something else, and when it
/// gives it back (retail, 721 ledgers; see [`Gear::put_away`] and [`Gear::give_back`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stow {
    /// A gather by hand: given back, in the hand at once, when the interaction ends (1,307 of
    /// the gathers that put a weapon away, 36 ms after the client's end).
    Gather,
    /// The inventory (3970 open): given back, drawn as by its weapon key, when it closes (385,
    /// 68 ms after the 3970 close).
    Inventory,
}

/// What a player wears and carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Gear {
    character: Uuid,
    /// Worn stacks in the order they were put on; `None`: a visible slot whose item was removed,
    /// kept in its place until an item fills it ([`EquippedItem::item`]).
    worn: Vec<(u32, Option<Stack>)>,
    /// One entry per bag slot.
    bag: Vec<Option<Stack>>,
    /// The sync reason of the empty record of each bag slot that a change of its own reason
    /// emptied ([`Gear::take_as`], [`Gear::take_from_slot_as`]); a slot forgets it when a
    /// stack fills it. Retail: a craft's emptied slot 2 (`Refining` 98.832 s,
    /// `Craft-Gypsum-Orbs` 16.524 s), a dye's 2 (`Hive_Dye` 232.245 s), a salvage's 24
    /// (`MSQ6-lv28` 888.480 s); other empty slots carry [`SYNC_LOGIN`].
    emptied: BTreeMap<usize, u8>,
    /// The version of the last states sent.
    version: u64,
    /// Whether the items changed since the states were sent.
    changed: bool,
    /// Whether the items changed since they were last taken for a save.
    unsaved: bool,
    /// The main-hand option slot drawn; `None`: every weapon is sheathed.
    drawn: Option<u32>,
    /// The main-hand option slot selected (the active map): the last one drawn, kept while every
    /// weapon is sheathed (retail, 721 ledgers: every attack-press draw from all sheathed, 334,
    /// drew it).
    active: u32,
    /// The drawn weapon reaches the hand at this time.
    attach_at_ms: Option<u64>,
    /// The drawn weapon is in the hand.
    attached: bool,
    /// The ammo of main-hand options 1 and 2.
    ammo: [Ammo; AMMO_OPTIONS],
    /// The items whose ammo [`Gear::arm`] last set, per option.
    armed: [Option<String>; AMMO_OPTIONS],
    /// The hands the own client was last told of: a change of the hands alone goes out as the
    /// children that differ from them ([`Gear::take_change`]).
    told: Hands,
    /// The option slot the server put away and gives back, and why.
    stowed: Option<(u32, Stow)>,
    /// The sheathe action puts the drawn weapon away at this time ([`Gear::sheathe`]).
    sheathe_at_ms: Option<u64>,
    /// The storage chests, the second container, the loadouts and the item counter.
    holdings: Holdings,
    /// The states last sent whole or changed: the next change goes out as the children that
    /// differ from them ([`PlayerGear::changes`]).
    sent: Option<PlayerGear>,
}

impl Gear {
    /// The gear of `character` with the backstory `backstory`: its login kit worn and an empty
    /// bag; `None` without a backstory or a kit of it.
    pub(super) fn starter(
        kits: &StarterKits,
        character: Uuid,
        backstory: Option<&str>,
    ) -> Option<Self> {
        let kit = kits.kit(backstory?)?;
        let mut gear = Self {
            character,
            worn: Vec::new(),
            bag: vec![None; STARTER_BAG_SLOTS],
            emptied: BTreeMap::new(),
            version: FIRST_GEAR_VERSION,
            changed: false,
            unsaved: false,
            drawn: None,
            active: FIRST_ACTIVE_OPTION,
            attach_at_ms: None,
            attached: false,
            ammo: [Ammo::default(); AMMO_OPTIONS],
            armed: [None, None],
            told: Hands::default(),
            stowed: None,
            sheathe_at_ms: None,
            holdings: Holdings::default(),
            sent: None,
        };
        for item in kit {
            if let Some(kind) = item.slot {
                let worn = gear.new_stack(item, 1);
                gear.put_on(first_slot(kind), worn);
            }
        }
        Some(gear.told_now())
    }

    /// The gear with each of `tools` worn in its slot; an item `kits` does not know is left out.
    pub(super) fn with_tools(mut self, kits: &StarterKits, tools: &[StartTool]) -> Self {
        for tool in tools {
            if let Some(item) = kits.item(&tool.item) {
                let worn = self.new_stack(item, 1);
                self.put_on(tool.slot, worn);
            }
        }
        self
    }

    /// The gear whose own client knows its hands as they are.
    fn told_now(mut self) -> Self {
        self.told = self.hands();
        self
    }

    /// The gear of `character` from the `items` another sim sent, with main-hand option slot
    /// `drawn` in the hand and option slot `active` selected (`None`: the drawn one, else the
    /// first); an item `kits` does not know is left out, a bag slot past the bag grows it, and an
    /// item whose bag slot is taken goes to a free one.
    pub(super) fn carried_in(
        kits: &StarterKits,
        character: Uuid,
        items: &[CarriedItem],
        drawn: Option<u32>,
        active: Option<u32>,
    ) -> Self {
        let mut gear = Self {
            character,
            worn: Vec::new(),
            bag: vec![None; STARTER_BAG_SLOTS],
            emptied: BTreeMap::new(),
            version: FIRST_GEAR_VERSION,
            changed: false,
            unsaved: false,
            drawn,
            active: drawn.or(active).unwrap_or(FIRST_ACTIVE_OPTION),
            attach_at_ms: None,
            attached: drawn.is_some(),
            ammo: [Ammo::default(); AMMO_OPTIONS],
            armed: [None, None],
            told: Hands::default(),
            stowed: None,
            sheathe_at_ms: None,
            holdings: Holdings::default(),
            sent: None,
        };
        for carried in items {
            let Some(item) = kits.item(&carried.item_id) else {
                continue;
            };
            let stack = gear.restored_stack(kits, item, carried.quantity, &carried.state);
            match carried.place {
                ItemPlace::Worn(slot_id) => gear.put_on(slot_id, stack),
                ItemPlace::Bag(bag_slot) => {
                    let wanted = usize::from(bag_slot);
                    if wanted < MAX_CONTAINER_SLOTS && gear.bag.len() <= wanted {
                        gear.bag.resize(wanted + 1, None);
                    }
                    match gear.bag.get_mut(wanted).filter(|entry| entry.is_none()) {
                        Some(entry) => *entry = Some(stack),
                        None => {
                            gear.put_in_bag(stack);
                        }
                    }
                }
            }
        }
        gear.told_now()
    }

    /// The items to carry to another sim.
    pub(super) fn carried(&self) -> Vec<CarriedItem> {
        let carried = |stack: &Stack, place| CarriedItem {
            item_id: stack.item_id.clone(),
            quantity: stack.item.quantity,
            place,
            state: stack.state(),
        };
        let worn = self
            .worn_stacks()
            .map(|(slot_id, stack)| carried(stack, ItemPlace::Worn(slot_id)));
        let bag = (0_u16..).zip(&self.bag).filter_map(|(bag_slot, entry)| {
            Some(carried(entry.as_ref()?, ItemPlace::Bag(bag_slot)))
        });
        worn.chain(bag).collect()
    }

    /// Marks the items unsaved, so the next save round writes them: the items of a handoff,
    /// which the sender did not save.
    pub(super) const fn mark_unsaved(&mut self) {
        self.unsaved = true;
    }

    /// The items to save when they changed since the last call; `None` when they did not.
    pub(super) fn take_unsaved(&mut self) -> Option<Vec<CarriedItem>> {
        if !self.unsaved {
            return None;
        }
        self.unsaved = false;
        Some(self.carried())
    }

    /// The whole gear states.
    pub(super) fn states(&self) -> PlayerGear {
        let equipped: Vec<EquippedItem> = self
            .worn
            .iter()
            .map(|(slot_id, stack)| EquippedItem {
                slot: *slot_id,
                key: stack
                    .as_ref()
                    .and_then(|stack| stack.key)
                    .unwrap_or_default(),
                item: stack.as_ref().map(|stack| stack.item),
            })
            .collect();
        let bag: Vec<Option<GearItem>> = self
            .bag
            .iter()
            .map(|stack| stack.as_ref().map(|stack| stack.item))
            .collect();
        PlayerGear::with_extras(&equipped, &bag, self.version, self.hands(), &self.extras())
    }

    /// Every stack of `item` becomes one new stack of `quantity` worn in paperdoll slot
    /// `slot_id`, at time `time_ns` ([`Gear::remove`]).
    pub(super) fn renew(&mut self, item: &KitItem, quantity: u32, slot_id: u32, time_ns: u64) {
        self.remove(&item.item_id, time_ns);
        self.worn
            .retain(|(worn, stack)| *worn != slot_id || stack.is_none());
        let renewed = self.new_stack(item, quantity);
        self.put_on(slot_id, renewed);
        self.changed = true;
        self.unsaved = true;
    }

    /// The worn stacks, without the emptied slots.
    fn worn_stacks(&self) -> impl Iterator<Item = (u32, &Stack)> {
        self.worn
            .iter()
            .filter_map(|(slot_id, stack)| Some((*slot_id, stack.as_ref()?)))
    }

    /// Wears `stack` in paperdoll slot `slot_id`: in the place of its emptied slot when it has
    /// one (retail 2026-09-18: the greatsword went into the sword's place, key 4), else last.
    /// A stack without an owned-items key takes the lowest free one.
    fn put_on(&mut self, slot_id: u32, mut stack: Stack) {
        if stack.key.is_none() {
            stack.key = Some(self.lowest_free_key());
        }
        match self
            .worn
            .iter_mut()
            .find(|(worn, kept)| *worn == slot_id && kept.is_none())
        {
            Some((_, kept)) => *kept = Some(stack),
            None => self.worn.push((slot_id, Some(stack))),
        }
    }

    /// Takes the worn entry at `index` off the list: a visible slot stays, emptied, in its place
    /// (retail 2026-09-18); a hidden one leaves the list. Its weapon leaves the hand: the option
    /// slot that was drawn, when it was.
    fn empty_worn(&mut self, index: usize) -> Option<u32> {
        let slot_id = self.worn[index].0;
        if is_visible_slot(slot_id) {
            self.worn[index].1 = None;
        } else {
            self.worn.remove(index);
        }
        let was_drawn = self.drawn == Some(slot_id);
        if was_drawn {
            self.drawn = None;
            self.attached = false;
            self.attach_at_ms = None;
            self.sheathe_at_ms = None;
        }
        if let Some(option) = ammo_index(slot_id) {
            self.ammo[option] = Ammo::default();
            self.armed[option] = None;
        }
        was_drawn.then_some(slot_id)
    }

    /// The ammo of main-hand option slot `slot_id` (options 1 and 2) takes the time it is loaded
    /// when put away at `time_ns`: that time plus its reload (retail, 721 ledgers: a melee or
    /// empty option took the server time, a blunderbuss option 1.46 s later, its
    /// `AutoReloadAmmoSeconds`; the count kept its shots).
    fn stamp(&mut self, slot_id: u32, time_ns: u64) {
        if let Some(index) = ammo_index(slot_id) {
            let ammo = &mut self.ammo[index];
            ammo.time_ns = time_ns.saturating_add(ammo.reload_ms.saturating_mul(NS_PER_MS));
        }
    }

    /// Uses one of the stack worn in paperdoll slot `slot_id`: its item id; `None` when the slot
    /// is empty. The last one empties the slot.
    pub(super) fn use_one(&mut self, slot_id: u32) -> Option<String> {
        let at = self
            .worn
            .iter()
            .position(|(worn, stack)| *worn == slot_id && stack.is_some())?;
        let stack = self.worn[at].1.as_mut()?;
        let item_id = stack.item_id.clone();
        stack.item.quantity = stack.item.quantity.saturating_sub(1);
        if stack.item.quantity == 0 {
            // A used-up stack is a consumable of a slot that is never drawn.
            let _ = self.empty_worn(at);
        }
        self.changed = true;
        self.unsaved = true;
        Some(item_id)
    }

    /// The item id worn in paperdoll slot `slot_id`.
    pub(super) fn worn_item(&self, slot_id: u32) -> Option<&str> {
        self.worn_stacks()
            .find(|(worn, _)| *worn == slot_id)
            .map(|(_, stack)| stack.item_id.as_str())
    }

    /// The perks of the worn items that apply, each with its item's gear score: those of every
    /// worn item but the weapons of the main-hand and off-hand options not in the hand (an
    /// off-hand option goes with its main-hand option: 27 with 24, 29 with 26).
    pub(super) fn worn_perks(&self) -> Vec<(&str, u32)> {
        let held = self.held_option();
        let in_hand = |slot_id: u32| match slot_id {
            slot::MAIN_HAND_OPTION_1..=slot::MAIN_HAND_OPTION_3 => held == Some(slot_id),
            slot::OFF_HAND_OPTION_1 | slot::OFF_HAND_OPTION_3 => {
                held.and_then(off_hand_of) == Some(slot_id)
            }
            _ => true,
        };
        self.worn_stacks()
            .filter(|(slot_id, _)| in_hand(*slot_id))
            .flat_map(|(_, stack)| {
                stack
                    .perks
                    .iter()
                    .map(|perk| (perk.as_str(), stack.item.gear_score))
            })
            .collect()
    }

    /// The off-hand item worn with the weapon in the hand (a shield).
    pub(super) fn off_hand_weapon(&self) -> Option<&str> {
        self.worn_item(off_hand_of(self.held_option()?)?)
    }

    /// The weapon in the hand, with its gear score: the item of the drawn option slot, else the
    /// worn main-hand item of the lowest option slot (Inferred: retail's first island hits are
    /// the slot 24 weapon's).
    pub(super) fn main_weapon(&self) -> Option<(&str, u32)> {
        let held = self.held_option()?;
        self.main_hand()
            .find(|(slot_id, _)| *slot_id == held)
            .map(|(_, stack)| (stack.item_id.as_str(), stack.item.gear_score))
    }

    /// The main-hand option slot in the hand: the drawn one when it holds a weapon, else the
    /// selected one ([`Gear::draw_to_attack`] draws it), else the lowest one that does.
    fn held_option(&self) -> Option<u32> {
        self.main_hand()
            .find(|(slot_id, _)| Some(*slot_id) == self.drawn)
            .or_else(|| {
                self.main_hand()
                    .find(|(slot_id, _)| *slot_id == self.active)
            })
            .or_else(|| self.main_hand().min_by_key(|(slot_id, _)| *slot_id))
            .map(|(slot_id, _)| slot_id)
    }

    /// The worn items of the main-hand option slots.
    fn main_hand(&self) -> impl Iterator<Item = (u32, &Stack)> {
        self.worn_stacks().filter(|(slot_id, _)| {
            (slot::MAIN_HAND_OPTION_1..=slot::MAIN_HAND_OPTION_3).contains(slot_id)
        })
    }

    /// The main-hand option slot drawn, when one is.
    pub(super) const fn drawn(&self) -> Option<u32> {
        self.drawn
    }

    /// The main-hand option slot selected, kept while every weapon is sheathed.
    pub(super) const fn active(&self) -> u32 {
        self.active
    }

    /// The player draws main-hand option `option` (0 for the first key) at `now_ms`: it is the
    /// selected option, its weapon reaches the hand [`ATTACH_MS`] later, and the option
    /// selected before and the other option of the picked one are stamped at `time_ns`
    /// ([`Gear::stamp`]). Retail, 721 ledgers: every one of 1,243 weapon-key swaps stamped the
    /// option put away; a key draw from all sheathed stamped the option selected before when it
    /// picked the other one (106 of 106) and both options when it picked the selected one (47 of
    /// 49). Whether it was a new draw.
    pub(super) fn draw(&mut self, option: u32, now_ms: u64, time_ns: u64) -> bool {
        let picked = slot::MAIN_HAND_OPTION_1.saturating_add(option);
        if picked > slot::MAIN_HAND_OPTION_3 || self.drawn == Some(picked) {
            return false;
        }
        self.stamp(self.active, time_ns);
        if let Some(paired) = paired_option(picked) {
            self.stamp(paired, time_ns);
        }
        self.drawn = Some(picked);
        self.active = picked;
        self.stowed = None;
        self.sheathe_at_ms = None;
        self.attached = false;
        self.attach_at_ms = Some(now_ms.saturating_add(ATTACH_MS));
        true
    }

    /// The player attacks with every weapon sheathed: the weapon of the selected option is drawn
    /// straight into the hand, the other option is stamped at `time_ns` ([`Gear::stamp`]), and
    /// the client is told. Retail, 721 ledgers: 344 attack presses drew from all sheathed, each
    /// the selected option (334 with the selection known, 96 of them option 2) and in the same
    /// state attached it (newchar 2026-09-12 and 2026-09-18: 32 and 35 ms after the first press
    /// of the ship fight, no weapon key); 341 stamped only the other option. Without it the
    /// client showed no weapon in the hand (live run 14). An empty selected option draws nothing
    /// (no retail case: in no ledger was option 1 empty while option 2 held a weapon). Whether
    /// it drew one.
    pub(super) fn draw_to_attack(&mut self, time_ns: u64) -> bool {
        if self.drawn.is_some() {
            return false;
        }
        self.stowed = None;
        self.draw_at_once(self.active, time_ns)
    }

    /// The player presses the sheathe key with every weapon sheathed at `now_ms`: the selected
    /// option is drawn as by its weapon key ([`Gear::draw`]; retail: 85 of 105 such draws were
    /// attached later with both options stamped). Whether it drew one.
    pub(super) fn draw_selected(&mut self, now_ms: u64, time_ns: u64) -> bool {
        if self.drawn.is_some() || self.worn_item(self.active).is_none() {
            return false;
        }
        self.active
            .checked_sub(slot::MAIN_HAND_OPTION_1)
            .is_some_and(|key| self.draw(key, now_ms, time_ns))
    }

    /// Draws main-hand option slot `picked` straight into the hand and stamps the other option
    /// at `time_ns`; whether it holds an item to draw.
    fn draw_at_once(&mut self, picked: u32, time_ns: u64) -> bool {
        if self.worn_item(picked).is_none() {
            return false;
        }
        if let Some(paired) = paired_option(picked) {
            self.stamp(paired, time_ns);
        }
        self.drawn = Some(picked);
        self.sheathe_at_ms = None;
        self.attach_at_ms = None;
        self.attached = true;
        true
    }

    /// The server puts the drawn weapon away at `time_ns`, the option stamped as a sheathe, to
    /// give it back for `back` ([`Gear::give_back`]; `None`: it stays away until the player
    /// draws). Retail, 721 ledgers, the own paperdoll 18 to 68 ms (medians) after the client's
    /// call, only the put-away option stamped (2,494 of 2,496): the inventory opening (3970 on:
    /// 639 of 641 with a weapon drawn), a gather by hand (1,351 of 1,449; with a tool 0 of 827),
    /// a talk (246 of 247), a lore note (115 of 116), a crafting station (6 of 6), a revive (90 of
    /// 92), a trigger (20 of 24), a cutscene request (6703: 43 of 44), the load of a new level
    /// (5181 after a change: 37 of 37) and a death (161 put-aways at the 4299). Whether a weapon
    /// was put away.
    pub(super) fn put_away(&mut self, time_ns: u64, back: Option<Stow>) -> bool {
        let Some(sheathed) = self.drawn.take() else {
            return false;
        };
        self.sheathe_at_ms = None;
        self.attached = false;
        self.attach_at_ms = None;
        self.stamp(sheathed, time_ns);
        self.stowed = back.map(|reason| (sheathed, reason));
        true
    }

    /// The sheathe action starts at `now_ms` (the sheathe key, deep water): it puts the drawn
    /// weapon away `after_ms` later, at the marker of its fragment ([`Stance::sheathe_ms`]), the
    /// option stamped as by [`Gear::put_away`]. Retail, 707 ledgers: 220 of 250 presses of the
    /// sheathe key with a weapon drawn put it away; the life staff (marker 733 ms) at 704 to
    /// 784 ms for 53 of 141 presses, never before 674 ms but one; the spear (marker 900 ms) at
    /// 1,031 ms and later (16 of 17). A new draw ends it (Inferred). Whether it started: `false`
    /// with every weapon sheathed or a put-away already coming.
    ///
    /// [`Stance::sheathe_ms`]: nw_content::player_attacks::Stance::sheathe_ms
    pub(super) fn sheathe(&mut self, now_ms: u64, after_ms: u64) -> bool {
        if self.drawn.is_none() || self.sheathe_at_ms.is_some() {
            return false;
        }
        self.sheathe_at_ms = Some(now_ms.saturating_add(after_ms));
        true
    }

    /// The reason `reason` of a put-away ends at `now_ms`: the option comes back when nothing
    /// was drawn since, after a gather in the hand at once with the other option stamped (as an
    /// attack's draw: 1,288 of 1,310), after the inventory as by its weapon key (attached
    /// [`ATTACH_MS`] later, both options stamped: 361 of 369). Whether it came back.
    pub(super) fn give_back(&mut self, reason: Stow, now_ms: u64, time_ns: u64) -> bool {
        let Some((option, stowed_for)) = self.stowed else {
            return false;
        };
        if stowed_for != reason {
            return false;
        }
        self.stowed = None;
        if self.drawn.is_some() {
            return false;
        }
        match reason {
            Stow::Gather => self.draw_at_once(option, time_ns),
            Stow::Inventory => option
                .checked_sub(slot::MAIN_HAND_OPTION_1)
                .is_some_and(|key| self.draw(key, now_ms, time_ns)),
        }
    }

    /// The weapon of main-hand option slot `slot_id` is in the client's hand already (its attack
    /// showed it): drawn and in the hand at once, the option put away keeping the time `time_ns`.
    /// The client is not told: a sent change makes it switch weapons (live 2026-09-18). Other
    /// players see the new hand ([`Gear::hands`]).
    pub(super) fn take_in_hand(&mut self, slot_id: u32, time_ns: u64) {
        let option = slot_id.saturating_sub(slot::MAIN_HAND_OPTION_1);
        if self.draw(option, 0, time_ns) {
            self.attach_at_ms = None;
            self.attached = true;
            self.told = self.hands();
        }
    }

    /// The lowest main-hand option slot whose item `wanted` picks.
    pub(super) fn option_with(&self, wanted: impl Fn(&str) -> bool) -> Option<u32> {
        self.main_hand()
            .filter(|(_, stack)| wanted(&stack.item_id))
            .map(|(slot_id, _)| slot_id)
            .min()
    }

    /// Gives main-hand options 1 and 2 the shots and reload time of their weapons by
    /// `weapon_of(item)`, at time `time_ns`: a new weapon in an option comes loaded.
    pub(super) fn arm(&mut self, weapon_of: impl Fn(&str) -> Option<(u8, u64)>, time_ns: u64) {
        for (index, slot_id) in (slot::MAIN_HAND_OPTION_1..).take(AMMO_OPTIONS).enumerate() {
            let item = self.worn_item(slot_id);
            if item == self.armed[index].as_deref() {
                continue;
            }
            let (max, reload_ms) = item.and_then(&weapon_of).unwrap_or_default();
            self.armed[index] = item.map(str::to_owned);
            let ammo = &mut self.ammo[index];
            if (ammo.max, ammo.reload_ms) != (max, reload_ms) {
                *ammo = Ammo {
                    max,
                    loaded: max,
                    reload_ms,
                    reload_at_ms: None,
                    time_ns,
                };
            }
        }
    }

    /// The weapon in the hand has shots but none is loaded: it reloads.
    pub(super) fn held_empty(&self) -> bool {
        self.held_option()
            .and_then(ammo_index)
            .is_some_and(|index| self.ammo[index].max > 0 && self.ammo[index].loaded == 0)
    }

    /// Fires one shot of the drawn option at `now_ms`; whether one was loaded. The last shot
    /// starts the reload (retail: loaded again 1.0 to 5 s after the second shot).
    pub(super) fn fire(&mut self, now_ms: u64) -> bool {
        let Some(index) = self.held_option().and_then(ammo_index) else {
            return false;
        };
        let ammo = &mut self.ammo[index];
        if ammo.loaded == 0 {
            return false;
        }
        ammo.loaded -= 1;
        if ammo.loaded == 0 {
            ammo.reload_at_ms = Some(now_ms.saturating_add(ammo.reload_ms));
        }
        true
    }

    /// Moves the hands on to `now_ms`, server time `time_ns` ([`unix_ns`]): the drawn weapon
    /// reaches the hand, the sheathe action puts it away at its marker, an empty weapon is loaded
    /// again.
    pub(super) fn tick_hands(&mut self, now_ms: u64, time_ns: u64) {
        if self.sheathe_at_ms.is_some_and(|at| now_ms >= at) {
            self.put_away(time_ns, None);
        }
        if self.attach_at_ms.is_some_and(|at| now_ms >= at) {
            self.attach_at_ms = None;
            self.attached = true;
        }
        for ammo in &mut self.ammo {
            if ammo.reload_at_ms.is_some_and(|at| now_ms >= at) {
                ammo.reload_at_ms = None;
                ammo.loaded = ammo.max;
            }
        }
    }

    /// How the player holds its weapons, for the paperdoll.
    /// The items that show on the player (armor and weapon options): what other players see.
    pub(super) fn visible_items(&self) -> Vec<EquippedItem> {
        self.worn
            .iter()
            .filter(|(slot_id, _)| is_visible_slot(*slot_id))
            .map(|(slot_id, stack)| EquippedItem {
                slot: *slot_id,
                key: stack
                    .as_ref()
                    .and_then(|stack| stack.key)
                    .unwrap_or_default(),
                item: stack.as_ref().map(|stack| stack.item),
            })
            .collect()
    }

    /// The version of the items other players see: the version of the states with the changes
    /// not yet taken. A new one is a change of the items other players must see; they see a
    /// change of the hands by the hands ([`Gear::hands`]).
    pub(super) const fn shown_version(&self) -> u64 {
        self.version + self.changed as u64
    }

    /// The weapon in the hand and the ammo.
    pub(super) fn hands(&self) -> Hands {
        Hands {
            drawn: self.drawn,
            active: Some(self.active),
            attached: self.attached,
            ammo: self.ammo.map(|ammo| {
                Some(LoadedAmmo {
                    count: ammo.loaded,
                    time_ns: ammo.time_ns,
                })
            }),
        }
    }

    /// Swaps the worn items of paperdoll slots `slot_a` and `slot_b`, which hold the items of
    /// instance `instance_a` and `instance_b` (all zero: the slot is empty); each must fit the
    /// other slot.
    pub(super) fn swap(
        &mut self,
        slot_a: u32,
        slot_b: u32,
        instance_a: [u8; RAW_ID_LEN],
        instance_b: [u8; RAW_ID_LEN],
    ) -> Result<(), EquipRefusal> {
        let holds = |gear: &Self, slot_id: u32, instance: [u8; RAW_ID_LEN]| match gear
            .worn_stacks()
            .find(|(worn, _)| *worn == slot_id)
        {
            Some((_, stack)) => stack.item.instance == instance,
            None => instance == [0; RAW_ID_LEN],
        };
        if !holds(self, slot_a, instance_a) || !holds(self, slot_b, instance_b) {
            return Err(EquipRefusal::NotWorn);
        }
        let fits_other = self.worn_stacks().all(|(slot_id, stack)| match slot_id {
            id if id == slot_a => fits(stack.kind, slot_b),
            id if id == slot_b => fits(stack.kind, slot_a),
            _ => true,
        });
        if !fits_other {
            return Err(EquipRefusal::WrongSlot);
        }
        for (slot_id, _) in &mut self.worn {
            if *slot_id == slot_a {
                *slot_id = slot_b;
            } else if *slot_id == slot_b {
                *slot_id = slot_a;
            }
        }
        self.changed = true;
        self.unsaved = true;
        Ok(())
    }

    /// Takes off the worn item of paperdoll slot `slot_id` into bag slot `bag_slot`, or the first
    /// free one when that is taken or out of range (`u32::MAX`: any).
    pub(super) fn unequip(&mut self, slot_id: u32, bag_slot: u32) -> Result<(), EquipRefusal> {
        if self.worn_item(slot_id).is_none() {
            return Err(EquipRefusal::NotWorn);
        }
        let wanted = usize::try_from(bag_slot)
            .ok()
            .filter(|index| self.bag.get(*index).is_some_and(Option::is_none));
        if wanted.is_none() && !self.bag_has_room() {
            return Err(EquipRefusal::BagFull);
        }
        let stack = self.take_off(slot_id).ok_or(EquipRefusal::NotWorn)?;
        match wanted {
            Some(index) => self.fill_bag_slot(index, stack),
            None => {
                self.put_in_bag(stack);
            }
        }
        self.changed = true;
        self.unsaved = true;
        Ok(())
    }

    /// The states after a change, once; `None` without a change. A change of the items sends the
    /// children that changed, a list as its changed elements, at a new version
    /// ([`PlayerGear::changes`]; retail newchar 2026-09-18: each equip and take); a change of
    /// the hands alone (a draw, a put-away, an attach, a shot, a reload) sends the hand children
    /// that changed and keeps the version ([`PlayerGear::hands_change`]). Live run 15 sent the
    /// whole states at the draw of an attack press and the client showed no weapon in the hand.
    pub(super) fn take_change(&mut self) -> Option<PlayerGear> {
        let hands = self.hands();
        let told = std::mem::replace(&mut self.told, hands);
        if self.changed {
            self.changed = false;
            self.version += 1;
            self.settle_keys();
            let now = self.states();
            let change = PlayerGear::changes(self.sent.as_ref(), &now, self.version);
            self.sent = Some(now);
            return Some(change);
        }
        let change = PlayerGear::hands_change(told, hands);
        if change.paperdoll.is_empty() {
            return None;
        }
        self.sent = Some(self.states());
        Some(change)
    }

    /// The whole states for the first own state: the client knows the hands and the items from
    /// then on.
    pub(super) fn first_states(&mut self) -> PlayerGear {
        self.told = self.hands();
        self.settle_keys();
        let states = self.states();
        self.sent = Some(states.clone());
        states
    }

    /// How many of `item_id` (any case) the bag holds.
    pub(super) fn bag_count(&self, item_id: &str) -> u32 {
        self.bag
            .iter()
            .flatten()
            .filter(|stack| stack.item_id.eq_ignore_ascii_case(item_id))
            .map(|stack| stack.item.quantity)
            .sum()
    }

    /// Takes `quantity` of `item_id` (any case) out of the bag, emptying the slots of stacks it
    /// uses up; whether the bag held that many (it changes nothing when not).
    pub(super) fn take(&mut self, item_id: &str, quantity: u32) -> bool {
        self.take_as(item_id, quantity, SYNC_MOVED)
    }

    /// [`Gear::take`] for a change of sync reason `reason` (a craft's
    /// [`nw_messages::items::SYNC_CRAFT_USED`]): each stack it takes from shows it.
    pub(in crate::world) fn take_as(&mut self, item_id: &str, quantity: u32, reason: u8) -> bool {
        if self.bag_count(item_id) < quantity {
            return false;
        }
        let mut left = quantity;
        for (index, entry) in self.bag.iter_mut().enumerate() {
            if left == 0 {
                break;
            }
            let Some(stack) = entry
                .as_mut()
                .filter(|stack| stack.item_id.eq_ignore_ascii_case(item_id))
            else {
                continue;
            };
            let taken = left.min(stack.item.quantity);
            stack.item.quantity -= taken;
            stack.reason = reason;
            left -= taken;
            if stack.item.quantity == 0 {
                *entry = None;
                self.emptied.insert(index, reason);
            }
        }
        self.changed = true;
        self.unsaved = true;
        true
    }

    /// The item in bag slot `bag_slot`: its item id and record; `None` for an empty slot.
    pub(super) fn bag_item(&self, bag_slot: u32) -> Option<(&str, &GearItem)> {
        let stack = self.bag.get(usize::try_from(bag_slot).ok()?)?.as_ref()?;
        Some((stack.item_id.as_str(), &stack.item))
    }

    /// Takes `quantity` of the stack in bag slot `bag_slot`, emptying the slot when the stack is
    /// used up; whether the slot held that many (it changes nothing when not).
    pub(super) fn take_from_slot(&mut self, bag_slot: u32, quantity: u32) -> bool {
        self.take_from_slot_marked(bag_slot, quantity, None)
    }

    /// [`Gear::take_from_slot`] for a change of sync reason `reason` (a dye's
    /// [`nw_messages::items::SYNC_CRAFT_USED`], a salvage's): the stack's record, or the
    /// emptied slot's, shows it.
    pub(super) fn take_from_slot_as(&mut self, bag_slot: u32, quantity: u32, reason: u8) -> bool {
        self.take_from_slot_marked(bag_slot, quantity, Some(reason))
    }

    /// [`Gear::take_from_slot`], marking the stack with `reason` when there is one.
    fn take_from_slot_marked(&mut self, bag_slot: u32, quantity: u32, reason: Option<u8>) -> bool {
        let Some(index) = usize::try_from(bag_slot).ok() else {
            return false;
        };
        let Some(entry) = self.bag.get_mut(index) else {
            return false;
        };
        let Some(stack) = entry
            .as_mut()
            .filter(|stack| quantity > 0 && stack.item.quantity >= quantity)
        else {
            return false;
        };
        stack.item.quantity -= quantity;
        if let Some(reason) = reason {
            stack.reason = reason;
        }
        if stack.item.quantity == 0 {
            *entry = None;
            if let Some(reason) = reason {
                self.emptied.insert(index, reason);
            }
        }
        self.changed = true;
        self.unsaved = true;
        true
    }

    /// Removes every stack of `item_id`, worn or in the bag, at time `time_ns`; whether there
    /// was one. A worn one leaves its visible slot emptied in its place, and its weapon leaves
    /// the hand.
    pub(super) fn remove(&mut self, item_id: &str, time_ns: u64) -> bool {
        let mut removed = false;
        while let Some(index) = self
            .worn
            .iter()
            .position(|(_, stack)| stack.as_ref().is_some_and(|stack| stack.item_id == item_id))
        {
            // The drawn weapon is put away: its option is stamped (retail newchar 2026-09-12 and
            // 2026-09-18: the tutorial sword's removal sent option 1's ammo 0 at the server time).
            if let Some(sheathed) = self.empty_worn(index) {
                self.stamp(sheathed, time_ns);
            }
            removed = true;
        }
        for entry in &mut self.bag {
            if entry.as_ref().is_some_and(|stack| stack.item_id == item_id) {
                *entry = None;
                removed = true;
            }
        }
        self.changed |= removed;
        self.unsaved |= removed;
        removed
    }

    /// Puts `quantity` of `item` into the bag: onto its stacks up to its `MaxStackSize`, the
    /// rest as new stacks in the first free slots, new slots when none is free (retail bags grew
    /// to 406 slots); whether all of it fit.
    pub(super) fn add(&mut self, item: &KitItem, quantity: u32) -> bool {
        self.add_as(item, quantity, SYNC_MOVED)
    }

    /// [`Gear::add`] for a change of sync reason `reason` (a craft's
    /// [`nw_messages::items::SYNC_CRAFTED`]): each stack it adds to or makes shows it.
    pub(in crate::world) fn add_as(&mut self, item: &KitItem, quantity: u32, reason: u8) -> bool {
        if quantity == 0 {
            return false;
        }
        let limit = item
            .max_stack
            .unwrap_or(MAX_STACK_QUANTITY)
            .clamp(1, MAX_STACK_QUANTITY);
        let mut left = quantity.min(MAX_STACK_QUANTITY);
        for stack in self
            .bag
            .iter_mut()
            .flatten()
            .filter(|stack| stack.item_id == item.item_id)
        {
            let added = limit.saturating_sub(stack.item.quantity).min(left);
            if added > 0 {
                stack.item.quantity += added;
                stack.reason = reason;
            }
            left -= added;
        }
        while left > 0 {
            let part = left.min(limit);
            let new = self.new_stack(item, part);
            if !self.put_in_bag_as(new, reason) {
                break;
            }
            left -= part;
        }
        self.changed = true;
        self.unsaved = true;
        left == 0
    }

    /// Equips `quantity` of the item of instance `instance` in bag slot `bag_slot` into paperdoll
    /// slot `slot_id`; an item worn there goes back to the bag. An item that `disabled` names
    /// (by item id) stays in the bag. The id of the equipped item.
    pub(super) fn equip(
        &mut self,
        bag_slot: u32,
        slot_id: u32,
        quantity: u32,
        instance: [u8; RAW_ID_LEN],
        disabled: impl Fn(&str) -> bool,
    ) -> Result<String, EquipRefusal> {
        let bag_index = usize::try_from(bag_slot).map_err(|_| EquipRefusal::NoSuchItem)?;
        let stack = self
            .bag
            .get(bag_index)
            .and_then(Option::as_ref)
            .filter(|stack| stack.item.instance == instance)
            .ok_or(EquipRefusal::NoSuchItem)?;
        if disabled(&stack.item_id) {
            return Err(EquipRefusal::Disabled);
        }
        if !fits(stack.kind, slot_id) {
            return Err(EquipRefusal::WrongSlot);
        }
        let moved = quantity.clamp(1, stack.item.quantity);
        let whole = moved == stack.item.quantity;
        let replaced = self
            .worn
            .iter()
            .position(|(worn, stack)| *worn == slot_id && stack.is_some());
        if replaced.is_some() && !whole && !self.bag_has_room() {
            return Err(EquipRefusal::BagFull);
        }
        let mut put_on = if whole {
            self.bag[bag_index].take().ok_or(EquipRefusal::NoSuchItem)?
        } else {
            let left = self.bag[bag_index]
                .as_mut()
                .ok_or(EquipRefusal::NoSuchItem)?;
            left.item.quantity -= moved;
            left.clone()
        };
        put_on.item.quantity = moved;
        let put_on = self.own_instance(put_on);
        let item_id = put_on.item_id.clone();
        // The item worn there leaves its place emptied for the new one (retail newchar
        // 2026-09-18: the class kit's chest replaced the wanderer's at its visible index).
        if replaced.is_some()
            && let Some(taken_off) = self.take_off(slot_id)
        {
            if whole {
                self.fill_bag_slot(bag_index, taken_off);
            } else {
                self.put_in_bag(taken_off);
            }
        }
        self.put_on(slot_id, put_on);
        self.changed = true;
        self.unsaved = true;
        Ok(item_id)
    }
}

/// The off-hand option slot that goes with main-hand option slot `option`: 27 with 24, 29
/// with 26.
const fn off_hand_of(option: u32) -> Option<u32> {
    match option {
        slot::MAIN_HAND_OPTION_1 => Some(slot::OFF_HAND_OPTION_1),
        slot::MAIN_HAND_OPTION_3 => Some(slot::OFF_HAND_OPTION_3),
        _ => None,
    }
}

/// A stack of `quantity` of `item` for `character`.
fn stack(character: Uuid, item: &KitItem, quantity: u32) -> Stack {
    Stack {
        item_id: item.item_id.clone(),
        kind: item.slot,
        item: GearItem {
            item_index: item.item_index,
            quantity,
            gear_score: item.gear_score,
            durability: item.durability,
            max_durability: item.durability,
            perks: item.perks,
            look: ItemLook::default(),
            counter: 0,
            bound: item.bound,
            locked: false,
            instance: instance_id(character, &item.item_id),
        },
        key: None,
        max_stack: item.max_stack,
        perks: item.perk_ids.clone(),
        reason: SYNC_MOVED,
    }
}

/// The instance id of `item_id` for `character`: derived from both, so it stays the same over
/// logins, handoffs, and container moves (the beach bag's records carry these ids).
pub(super) fn instance_id(character: Uuid, item_id: &str) -> [u8; RAW_ID_LEN] {
    *Uuid::new_v5(&character, item_id.as_bytes()).as_bytes()
}

#[cfg(test)]
pub(crate) mod tests {
    use nw_content::starter::{ItemRow, StarterSource};

    use super::*;
    use nw_messages::paperdoll::{ItemVisual, SlotBits};
    use nw_messages::{ContainerState, OwnedItems, ReplicatedList};

    pub(crate) const BACKSTORY: &str = "Archetype_Mystic";
    pub(crate) const CHARACTER: Uuid = Uuid::from_u128(0x2b87_684d_c82c_46a4_90e4_c764_0247_7507);
    pub(crate) const SWORD: &str = "1hSwordT2_FTUE";
    pub(crate) const STAFF: &str = "MysticArchetype_2hStaffLife";
    pub(crate) const POTION: &str = "PotionHealthT1";
    /// A resource the kit does not give: the bag takes it from a gather.
    pub(crate) const FLINT: &str = "FlintT1";
    const POTION_SLOT: u32 = 14;
    const POTIONS: u32 = 2;

    /// No item is disabled.
    pub(crate) fn none_disabled(_item_id: &str) -> bool {
        false
    }

    /// A kit of [`BACKSTORY`]: a chest and the sword at login, the staff in the bag.
    pub(crate) fn kits() -> StarterKits {
        kits_with(&[])
    }

    /// The kit of [`BACKSTORY`] with its bag's staff worn in the second main-hand option: two
    /// weapons of two masteries (kill rewards).
    pub(crate) fn gear_with_staff() -> Gear {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("kit");
        for item in kits.bag(BACKSTORY) {
            assert!(gear.add(item, 1));
        }
        gear.equip(
            0,
            slot::MAIN_HAND_OPTION_1 + 1,
            1,
            instance(STAFF),
            none_disabled,
        )
        .expect("staff");
        gear
    }

    #[test]
    fn the_worn_perks_are_those_of_the_armor_and_the_weapon_in_the_hand() {
        let mut gear = gear_with_staff();
        for (slot_id, stack) in gear.worn.iter_mut() {
            if let Some(stack) = stack {
                stack.perks = vec![format!("Perk_{slot_id}")];
            }
        }
        let held = gear.held_option().expect("a weapon");
        let mut listed: Vec<String> = gear
            .worn_perks()
            .into_iter()
            .map(|(perk, _)| perk.to_owned())
            .collect();
        listed.sort();
        let mut wanted: Vec<String> = gear
            .worn_stacks()
            .map(|(slot_id, _)| slot_id)
            .filter(|slot_id| {
                !(slot::MAIN_HAND_OPTION_1..=slot::OFF_HAND_OPTION_3).contains(slot_id)
                    || *slot_id == held
            })
            .map(|slot_id| format!("Perk_{slot_id}"))
            .collect();
        wanted.sort();
        assert_eq!(listed, wanted);
        assert!(
            listed.contains(&format!("Perk_{held}")),
            "the weapon in the hand"
        );
        let weapons = gear.main_hand().count();
        assert!(weapons >= 2, "the kit's weapon and the staff");
    }

    /// [`kits`] with the master rows of `extra` items too: (item id, item class).
    pub(crate) fn kits_with(extra: &[(&str, &str)]) -> StarterKits {
        let extra: Vec<(&str, &str, Option<u32>)> = extra
            .iter()
            .map(|(item_id, item_class)| (*item_id, *item_class, None))
            .collect();
        kits_with_stacks(&extra)
    }

    /// [`kits_with`] for `extra` items with a stack size: (item id, item class, `MaxStackSize`).
    pub(crate) fn kits_with_stacks(extra: &[(&str, &str, Option<u32>)]) -> StarterKits {
        let row = |item_id: &str, item_class: &str| ItemRow {
            item_id: item_id.to_owned(),
            item_class: Some(item_class.to_owned()),
            gear_score: Some(75),
            durability: Some(375),
            bind_on_pickup: true,
            ..ItemRow::default()
        };
        let mut rows = vec![
            row("MysticArchetypeChest", "EquippableChest+Armor+Light"),
            row(SWORD, "Weapon+EquippableMainHand+Sword+Melee"),
            row(STAFF, "Weapon+EquippableTwoHand+LifeStaff+Magic"),
            row(POTION, "Consumable+Potion"),
            // A resource as the data has it (`FlintT1`): no durability, not bound, it stacks.
            ItemRow {
                durability: None,
                bind_on_pickup: false,
                ..row(FLINT, "Resource")
            },
        ];
        rows.extend(
            extra
                .iter()
                .map(|(item_id, item_class, max_stack)| ItemRow {
                    max_stack: *max_stack,
                    ..row(item_id, item_class)
                }),
        );
        StarterKits::build(StarterSource {
            item_sheets: vec![("MasterItemDefinitions_Quest".to_owned(), rows)],
            backstories: vec![(
                BACKSTORY.to_owned(),
                format!("MysticArchetypeChest:1,{SWORD}:1,{STAFF}:0"),
            )],
            perk_indexes: std::collections::HashMap::new(),
        })
    }

    /// The whole elements of a list child of whole states.
    fn whole<T: Clone>(list: Option<&ReplicatedList<T>>) -> Vec<T> {
        list.and_then(ReplicatedList::full)
            .expect("a whole list")
            .elements
            .clone()
    }

    fn visible_slots(gear: &PlayerGear) -> Vec<u32> {
        whole(
            gear.paperdoll
                .public
                .as_ref()
                .and_then(|public| public.visible_slots.as_ref()),
        )
    }

    /// The owned-items keys of whole states.
    fn owned_keys(gear: &PlayerGear) -> Vec<u16> {
        gear.owned
            .owned
            .as_ref()
            .and_then(OwnedItems::full)
            .expect("owned")
            .elements
            .iter()
            .map(|owned| owned.key)
            .collect()
    }

    fn instance(item_id: &str) -> [u8; RAW_ID_LEN] {
        *Uuid::new_v5(&CHARACTER, item_id.as_bytes()).as_bytes()
    }

    #[test]
    fn a_test_world_start_tool_is_worn_in_its_tool_slot() {
        const AXE: &str = "LoggingAxeT1";
        const LOGGING_SLOT: u32 = 33;
        let kits = kits_with(&[(AXE, "Tool+LoggingAxe")]);
        let tools = [
            StartTool {
                item: AXE.to_owned(),
                slot: LOGGING_SLOT,
            },
            StartTool {
                item: "NoSuchTool".to_owned(),
                slot: LOGGING_SLOT + 1,
            },
        ];
        let gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY))
            .expect("gear")
            .with_tools(&kits, &tools);
        assert_eq!(gear.worn_item(LOGGING_SLOT), Some(AXE));
        assert_eq!(
            gear.worn_item(LOGGING_SLOT + 1),
            None,
            "an unknown item is left out"
        );
    }

    #[test]
    fn the_kit_is_worn_with_stable_instance_ids() {
        let kits = kits();
        let gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        let states = gear.states();
        assert_eq!(
            visible_slots(&states),
            vec![slot::CHEST, slot::MAIN_HAND_OPTION_1]
        );
        let again = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        assert_eq!(again.states(), states, "the same ids at the next login");
        assert_eq!(Gear::starter(&kits, CHARACTER, None), None);
        assert_eq!(
            Gear::starter(&kits, CHARACTER, Some("Archetype_Nobody")),
            None
        );
    }

    #[test]
    fn a_craft_takes_ingredients_out_of_the_bag_only() {
        const FLINTS: u32 = 3;
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        let flint = kits.item(FLINT).expect("flint");
        assert!(gear.add(flint, FLINTS));
        assert_eq!(gear.bag_count("flintt1"), FLINTS);
        assert!(!gear.take(FLINT, FLINTS + 1), "not that many");
        assert_eq!(gear.bag_count(FLINT), FLINTS, "nothing taken");
        assert!(gear.take(FLINT, 2));
        assert_eq!(gear.bag_count(FLINT), 1);
        assert!(gear.take(FLINT, 1));
        assert_eq!(gear.bag_count(FLINT), 0, "the slot is empty");
        assert!(gear.carried().iter().all(|item| item.item_id != FLINT));
        assert_eq!(gear.bag_count(SWORD), 0, "a worn item is not in the bag");
        assert!(!gear.take(SWORD, 1));
    }

    #[test]
    fn the_tutorial_takes_the_sword_and_equips_from_the_bag() {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        assert_eq!(gear.take_change(), None);
        assert!(gear.remove(SWORD, 0));
        assert!(!gear.remove(SWORD, 0));
        let after = gear.take_change().expect("change");
        assert_eq!(
            visible_slots(&after),
            vec![slot::CHEST, slot::MAIN_HAND_OPTION_1],
            "retail keeps the sword's slot, emptied"
        );
        for item in kits.bag(BACKSTORY) {
            assert!(gear.add(item, 1));
        }
        let potion = kits.item(POTION).expect("potion");
        assert!(gear.add(potion, 1));
        assert!(gear.add(potion, 1), "onto the same stack");
        assert_eq!(
            gear.equip(0, slot::CHEST, 1, instance(STAFF), none_disabled),
            Err(EquipRefusal::WrongSlot)
        );
        assert_eq!(
            gear.equip(
                0,
                slot::MAIN_HAND_OPTION_1,
                1,
                instance(POTION),
                none_disabled
            ),
            Err(EquipRefusal::NoSuchItem)
        );
        assert_eq!(
            gear.equip(
                0,
                slot::MAIN_HAND_OPTION_1,
                1,
                instance(STAFF),
                none_disabled
            ),
            Ok(STAFF.to_owned())
        );
        assert_eq!(
            gear.equip(1, POTION_SLOT, POTIONS, instance(POTION), none_disabled),
            Ok(POTION.to_owned())
        );
        assert!(gear.take_change().is_some());
        let states = gear.states();
        assert_eq!(
            visible_slots(&states),
            vec![slot::CHEST, slot::MAIN_HAND_OPTION_1]
        );
        let hidden = whole(
            states
                .paperdoll
                .owner
                .as_ref()
                .and_then(|owner| owner.hidden_items.as_ref()),
        );
        assert_eq!(hidden[0].quantity, POTIONS);
        assert!(
            gear.bag.iter().all(Option::is_none),
            "the bag is empty again"
        );
        assert_eq!(
            gear.main_weapon(),
            Some((STAFF, 75)),
            "the staff in the hand"
        );
        // Drinking: one potion less per use; the last one empties the slot.
        assert_eq!(gear.worn_item(POTION_SLOT), Some(POTION));
        assert_eq!(gear.use_one(POTION_SLOT), Some(POTION.to_owned()));
        assert!(gear.take_change().is_some());
        assert_eq!(gear.use_one(POTION_SLOT), Some(POTION.to_owned()));
        assert_eq!(gear.worn_item(POTION_SLOT), None);
        assert_eq!(gear.use_one(POTION_SLOT), None);
        // The quest's end: a fresh stack of potions in the quick slot.
        gear.renew(potion, POTIONS, POTION_SLOT, 0);
        assert_eq!(gear.worn_item(POTION_SLOT), Some(POTION));
        assert_eq!(gear.use_one(POTION_SLOT), Some(POTION.to_owned()));
        assert_eq!(gear.worn_item(POTION_SLOT), Some(POTION), "one left");
    }

    #[test]
    fn a_removed_weapon_leaves_the_hand_and_its_place_for_the_next() {
        const DRAWN_NS: u64 = 1;
        const REMOVED_NS: u64 = 2;
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        gear.take_in_hand(slot::MAIN_HAND_OPTION_1, DRAWN_NS);
        assert!(gear.remove(SWORD, REMOVED_NS));
        let after = gear.take_change().expect("change");
        let public = after.paperdoll.public.as_ref().expect("public");
        assert_eq!(whole(public.visuals.as_ref())[1], ItemVisual::of(0));
        assert_eq!(
            gear.hands(),
            Hands {
                drawn: None,
                active: Some(slot::MAIN_HAND_OPTION_1),
                attached: false,
                ammo: [
                    Some(LoadedAmmo {
                        count: 0,
                        time_ns: REMOVED_NS
                    }),
                    Some(LoadedAmmo {
                        count: 0,
                        time_ns: DRAWN_NS
                    }),
                ],
            },
            "nothing in the hand; the put-away option stamped at the removal (retail 2026-09-18: \
             option 1's ammo 0 at the server time), still the selected one"
        );
        assert_eq!(gear.main_weapon(), None);
        assert!(
            gear.carried().iter().all(|item| item.item_id != SWORD),
            "not saved"
        );
        // The next weapon goes into the sword's place: owned key 1, as retail's greatsword.
        for item in kits.bag(BACKSTORY) {
            assert!(gear.add(item, 1));
        }
        assert_eq!(
            gear.equip(
                0,
                slot::MAIN_HAND_OPTION_1,
                1,
                instance(STAFF),
                none_disabled
            ),
            Ok(STAFF.to_owned())
        );
        assert!(gear.take_change().is_some());
        let states = gear.states();
        assert_eq!(
            visible_slots(&states),
            vec![slot::CHEST, slot::MAIN_HAND_OPTION_1]
        );
        assert_eq!(owned_keys(&states), [0, 1]);
    }

    #[test]
    fn carried_items_come_back_the_same() {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        for item in kits.bag(BACKSTORY) {
            gear.add(item, 1);
        }
        let mut carried = gear.carried();
        let again = Gear::carried_in(&kits, CHARACTER, &carried, None, None);
        assert_eq!(again.states(), gear.states());
        carried.push(CarriedItem {
            item_id: "NoSuchItem".to_owned(),
            quantity: 1,
            place: ItemPlace::Bag(0),
            state: nw_bus::ItemState::default(),
        });
        let potion = CarriedItem {
            item_id: POTION.to_owned(),
            quantity: 1,
            place: ItemPlace::Bag(0),
            state: nw_bus::ItemState::default(),
        };
        carried.push(potion);
        let moved = Gear::carried_in(&kits, CHARACTER, &carried, None, None);
        assert_eq!(
            moved.bag[1].as_ref().map(|stack| stack.item_id.as_str()),
            Some(POTION),
            "a taken bag slot sends it to a free one"
        );
    }

    #[test]
    fn a_change_of_items_is_taken_for_a_save_once() {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        assert_eq!(gear.take_unsaved(), None, "a new kit has nothing to save");
        gear.draw(slot::MAIN_HAND_OPTION_1, 0, 0);
        assert_eq!(
            gear.take_unsaved(),
            None,
            "drawing a weapon changes no item"
        );
        let flint = kits.item(FLINT).expect("flint").clone();
        assert!(gear.add(&flint, 1));
        assert_eq!(gear.take_unsaved(), Some(gear.carried()));
        assert_eq!(gear.take_unsaved(), None);
        assert!(gear.remove(FLINT, 0));
        assert!(gear.take_unsaved().is_some());
    }

    #[test]
    fn a_stack_holds_one_to_the_largest_stored_quantity() {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        let flint = kits.item(FLINT).expect("flint").clone();
        assert!(!gear.add(&flint, 0), "no stack of nothing");
        assert!(gear.add(&flint, MAX_STACK_QUANTITY));
        assert!(gear.add(&flint, 1));
        let stacks: Vec<u32> = gear
            .carried()
            .iter()
            .filter(|stack| stack.item_id == FLINT)
            .map(|stack| stack.quantity)
            .collect();
        assert_eq!(
            stacks,
            [MAX_STACK_QUANTITY, 1],
            "the database stores at most i32::MAX per stack; the rest starts a new one"
        );
    }

    #[test]
    fn the_drawn_weapon_goes_with_the_items_to_another_sim() {
        const SECOND_OPTION: u32 = slot::MAIN_HAND_OPTION_1 + 1;
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        gear.draw(1, 0, 0);
        assert_eq!(gear.drawn(), Some(SECOND_OPTION));
        let taken = Gear::carried_in(
            &kits,
            CHARACTER,
            &gear.carried(),
            gear.drawn(),
            Some(gear.active()),
        );
        assert_eq!(taken.drawn(), Some(SECOND_OPTION));
        assert!(taken.hands().attached, "already in the hand");
        let fresh = Gear::carried_in(&kits, CHARACTER, &gear.carried(), None, None);
        assert_eq!(fresh.drawn(), None);
        assert_eq!(fresh.active(), slot::MAIN_HAND_OPTION_1, "the first one");
        // Sheathed on the other sim, the second option stays the selected one.
        let sheathed =
            Gear::carried_in(&kits, CHARACTER, &gear.carried(), None, Some(gear.active()));
        assert_eq!(sheathed.drawn(), None);
        assert_eq!(sheathed.active(), SECOND_OPTION);
    }

    #[test]
    fn a_worn_item_goes_back_to_the_bag_slot() {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        for item in kits.bag(BACKSTORY) {
            gear.add(item, 1);
        }
        assert_eq!(
            gear.equip(
                0,
                slot::MAIN_HAND_OPTION_1,
                1,
                instance(STAFF),
                none_disabled
            ),
            Ok(STAFF.to_owned())
        );
        let back = gear.bag[0].as_ref().expect("the sword is in the bag");
        assert_eq!(back.item_id, SWORD);
    }

    #[test]
    fn a_drawn_weapon_reaches_the_hand_and_a_shot_weapon_reloads() {
        const SHOTS: u8 = 2;
        const RELOAD_MS: u64 = 1_460;
        const NOW: u64 = 10_000;
        const TIME_NS: u64 = 7;
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        // The kit sword is a two-shot weapon here.
        gear.arm(
            |item| (item == SWORD).then_some((SHOTS, RELOAD_MS)),
            TIME_NS,
        );
        let hands = gear.hands();
        assert_eq!(hands.drawn, None, "every weapon sheathed at login");
        assert_eq!(
            hands.ammo[0],
            Some(LoadedAmmo {
                count: SHOTS,
                time_ns: TIME_NS
            })
        );
        gear.draw(0, NOW, TIME_NS);
        assert_eq!(gear.hands().drawn, Some(slot::MAIN_HAND_OPTION_1));
        assert!(!gear.hands().attached);
        gear.tick_hands(NOW + ATTACH_MS, 0);
        assert!(gear.hands().attached, "in the hand after the attach time");
        assert!(gear.fire(NOW));
        assert!(gear.fire(NOW));
        assert!(!gear.fire(NOW), "empty after two shots");
        assert_eq!(gear.hands().ammo[0].map(|ammo| ammo.count), Some(0));
        gear.tick_hands(NOW + RELOAD_MS - 1, 0);
        assert!(!gear.fire(NOW + RELOAD_MS - 1));
        gear.tick_hands(NOW + RELOAD_MS, 0);
        assert_eq!(gear.hands().ammo[0].map(|ammo| ammo.count), Some(SHOTS));
        // The same weapon again keeps its shots; arming checks only changed slots.
        assert!(gear.fire(NOW + RELOAD_MS));
        gear.arm(|_| Some((SHOTS, RELOAD_MS)), TIME_NS);
        assert_eq!(gear.hands().ammo[0].map(|ammo| ammo.count), Some(SHOTS - 1));
    }

    /// The kit gear with its sword a gun of `shots` shots and a reload of `reload_ms`, armed at
    /// `time_ns`.
    fn gun_kit_gear(shots: u8, reload_ms: u64, time_ns: u64) -> Gear {
        let mut gear = Gear::starter(&kits(), CHARACTER, Some(BACKSTORY)).expect("gear");
        gear.arm(
            |item| (item == SWORD).then_some((shots, reload_ms)),
            time_ns,
        );
        gear
    }

    #[test]
    fn an_attack_with_every_weapon_sheathed_draws_it_into_the_hand_and_tells_the_client() {
        /// Retail newchar 2026-09-12 at 1792415139441 and 2026-09-18 at 1792695769648, 32 and
        /// 35 ms after the first attack press of the ship fight: the own paperdoll's sheathed map
        /// (slots 9, 25 to 29) and attached map (slot 24).
        const RETAIL_SHEATHED: SlotBits = SlotBits([0, 2, 0, 62, 0, 0, 0]);
        const RETAIL_ATTACHED: SlotBits = SlotBits([0, 0, 0, 1, 0, 0, 0]);
        const NOW_NS: u64 = 1_792_695_769_624_369_200;
        const LATER_NS: u64 = NOW_NS + 1;
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        assert_eq!(gear.take_change(), None);
        assert!(gear.draw_to_attack(NOW_NS));
        let states = gear.take_change().expect("the client is told");
        let public = states.paperdoll.public.as_ref().expect("public");
        assert_eq!(public.sheathed, Some(RETAIL_SHEATHED));
        assert_eq!(
            public.attached,
            Some(RETAIL_ATTACHED),
            "in the hand at once"
        );
        assert_eq!(
            gear.hands().ammo,
            [
                Some(LoadedAmmo {
                    count: 0,
                    time_ns: 0
                }),
                Some(LoadedAmmo {
                    count: 0,
                    time_ns: NOW_NS
                }),
            ],
            "only the other option is stamped (retail a2=0 at the server time)"
        );
        assert!(!gear.draw_to_attack(LATER_NS), "a drawn weapon stays");
        assert_eq!(gear.take_change(), None);
    }

    #[test]
    fn an_attack_with_every_weapon_sheathed_draws_the_selected_option() {
        const SECOND_OPTION: u32 = slot::MAIN_HAND_OPTION_1 + 1;
        const NOW_NS: u64 = 5;
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        assert!(gear.add(kits.item(STAFF).expect("staff"), 1));
        assert_eq!(
            gear.equip(0, SECOND_OPTION, 1, instance(STAFF), none_disabled),
            Ok(STAFF.to_owned())
        );
        // Selected on another sim, every weapon sheathed: the second option is drawn.
        let mut moved =
            Gear::carried_in(&kits, CHARACTER, &gear.carried(), None, Some(SECOND_OPTION));
        assert!(moved.draw_to_attack(NOW_NS));
        assert_eq!(moved.drawn(), Some(SECOND_OPTION));
        assert_eq!(moved.main_weapon().map(|(item, _)| item), Some(STAFF));
        assert_eq!(
            moved.hands().ammo[0].map(|ammo| ammo.time_ns),
            Some(NOW_NS),
            "the first option is the other one"
        );
        // An empty selected option draws nothing.
        let mut empty =
            Gear::carried_in(&kits, CHARACTER, &gear.carried(), None, Some(SECOND_OPTION));
        assert!(empty.remove(STAFF, NOW_NS));
        assert!(!empty.draw_to_attack(NOW_NS));
        assert_eq!(empty.drawn(), None);
    }

    #[test]
    fn a_put_away_gun_is_stamped_when_it_is_loaded_again() {
        /// The retail blunderbuss: `MaxLoadedAmmo` 2, `AutoReloadAmmoSeconds` 1.46.
        const SHOTS: u8 = 2;
        const RELOAD_MS: u64 = 1_460;
        const ARMED_NS: u64 = 1;
        const KEY_NS: u64 = 1_000_000_000;
        // Retail newchar 2026-09-18 at 1792695954760: the second weapon key put the blunderbuss
        // away with its 2 shots, loaded at the server time plus 1.46 s.
        let mut gear = gun_kit_gear(SHOTS, RELOAD_MS, ARMED_NS);
        assert!(gear.draw(0, 0, ARMED_NS));
        assert!(gear.draw(1, 0, KEY_NS));
        assert_eq!(
            gear.hands().ammo[0],
            Some(LoadedAmmo {
                count: SHOTS,
                time_ns: KEY_NS + RELOAD_MS * NS_PER_MS
            })
        );
    }

    #[test]
    fn a_weapon_key_from_all_sheathed_stamps_the_selected_and_the_other_option() {
        const FIRST_NS: u64 = 3;
        const SECOND_NS: u64 = 4;
        // The selected option picked again (retail: both options stamped, 47 of 49).
        let mut gear = Gear::starter(&kits(), CHARACTER, Some(BACKSTORY)).expect("gear");
        assert!(gear.draw(0, 0, FIRST_NS));
        let stamps = |gear: &Gear| gear.hands().ammo.map(|ammo| ammo.map(|ammo| ammo.time_ns));
        assert_eq!(stamps(&gear), [Some(FIRST_NS), Some(FIRST_NS)]);
        // A swap stamps the option put away only (retail: 1,243 of 1,243).
        assert!(gear.draw(1, 0, SECOND_NS));
        assert_eq!(stamps(&gear), [Some(SECOND_NS), Some(FIRST_NS)]);
        assert_eq!(gear.active(), slot::MAIN_HAND_OPTION_1 + 1);
    }

    #[test]
    fn a_gather_puts_the_weapon_away_and_gives_it_back_in_the_hand() {
        const DRAWN_NS: u64 = 1;
        const AWAY_NS: u64 = 2;
        const BACK_NS: u64 = 3;
        const NOW: u64 = 10;
        let stamps = |gear: &Gear| gear.hands().ammo.map(|ammo| ammo.map(|ammo| ammo.time_ns));
        let mut gear = Gear::starter(&kits(), CHARACTER, Some(BACKSTORY)).expect("gear");
        assert!(gear.draw_to_attack(DRAWN_NS));
        assert!(gear.take_change().is_some());
        assert!(gear.put_away(AWAY_NS, Some(Stow::Gather)));
        let away = gear.take_change().expect("the client is told");
        let public = away.paperdoll.public.as_ref().expect("public");
        assert_eq!(public.sheathed, Some(SlotBits([0, 2, 0, 63, 0, 0, 0])));
        assert_eq!(public.attached, Some(SlotBits([0; 7])));
        assert_eq!(
            stamps(&gear),
            [Some(AWAY_NS), Some(DRAWN_NS)],
            "the put-away option"
        );
        assert!(
            !gear.give_back(Stow::Inventory, NOW, BACK_NS),
            "not the inventory's"
        );
        assert!(gear.give_back(Stow::Gather, NOW, BACK_NS));
        assert_eq!(gear.hands().drawn, Some(slot::MAIN_HAND_OPTION_1));
        assert!(gear.hands().attached, "in the hand at once");
        assert_eq!(
            stamps(&gear),
            [Some(AWAY_NS), Some(BACK_NS)],
            "the other option"
        );
        assert!(!gear.give_back(Stow::Gather, NOW, BACK_NS), "once");
    }

    #[test]
    fn the_inventory_gives_the_weapon_back_as_its_weapon_key() {
        const AWAY_NS: u64 = 2;
        const BACK_NS: u64 = 3;
        const NOW: u64 = 10;
        let mut gear = Gear::starter(&kits(), CHARACTER, Some(BACKSTORY)).expect("gear");
        assert!(gear.draw_to_attack(1));
        assert!(gear.put_away(AWAY_NS, Some(Stow::Inventory)));
        assert!(gear.give_back(Stow::Inventory, NOW, BACK_NS));
        let hands = gear.hands();
        assert_eq!(hands.drawn, Some(slot::MAIN_HAND_OPTION_1));
        assert!(!hands.attached, "attached later");
        assert_eq!(
            hands.ammo.map(|ammo| ammo.map(|ammo| ammo.time_ns)),
            [Some(BACK_NS), Some(BACK_NS)],
            "both options stamped"
        );
        gear.tick_hands(NOW + ATTACH_MS, 0);
        assert!(gear.hands().attached);
        // Put away for good, or drawn again by the player meanwhile: nothing comes back.
        assert!(gear.put_away(AWAY_NS, None));
        assert!(!gear.give_back(Stow::Inventory, NOW, BACK_NS));
        assert!(
            gear.draw_selected(NOW, BACK_NS),
            "the sheathe key draws the selected one"
        );
        assert!(gear.put_away(AWAY_NS, Some(Stow::Gather)));
        assert!(gear.draw_selected(NOW, BACK_NS));
        assert!(
            !gear.give_back(Stow::Gather, NOW, BACK_NS),
            "the player drew it"
        );
        assert!(!gear.draw_selected(NOW, BACK_NS), "already drawn");
    }

    #[test]
    fn the_sheathe_action_puts_the_weapon_away_at_its_marker_once() {
        const MARKER_MS: u64 = 400;
        const NOW: u64 = 10;
        const AWAY_NS: u64 = 5;
        let mut gear = Gear::starter(&kits(), CHARACTER, Some(BACKSTORY)).expect("gear");
        assert!(!gear.sheathe(NOW, MARKER_MS), "nothing drawn");
        assert!(gear.draw_to_attack(1));
        assert!(gear.take_change().is_some());
        assert!(gear.sheathe(NOW, MARKER_MS));
        assert!(!gear.sheathe(NOW + 1, MARKER_MS), "one put-away at a time");
        gear.tick_hands(NOW + MARKER_MS - 1, AWAY_NS);
        assert_eq!(gear.drawn(), Some(slot::MAIN_HAND_OPTION_1));
        gear.tick_hands(NOW + MARKER_MS, AWAY_NS);
        assert_eq!(gear.drawn(), None);
        assert_eq!(
            gear.hands().ammo[0].map(|ammo| ammo.time_ns),
            Some(AWAY_NS),
            "stamped as a put-away"
        );
        assert!(gear.take_change().is_some(), "the client is told");
        // A new draw before the marker ends the action.
        assert!(gear.draw_to_attack(1));
        assert!(gear.sheathe(NOW, MARKER_MS));
        assert!(gear.draw(1, NOW, 1));
        gear.tick_hands(NOW + MARKER_MS, AWAY_NS);
        assert_eq!(gear.drawn(), Some(slot::MAIN_HAND_OPTION_1 + 1));
    }

    #[test]
    fn other_players_see_the_weapon_the_hand_took_from_an_attack() {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        let items = gear.shown_version();
        let before = gear.hands();
        gear.take_in_hand(slot::MAIN_HAND_OPTION_1, 1);
        assert_eq!(gear.take_change(), None, "the own client is not told");
        assert_ne!(gear.hands(), before, "other players see the new hand");
        assert_eq!(gear.hands().drawn, Some(slot::MAIN_HAND_OPTION_1));
        assert_eq!(gear.shown_version(), items, "the items did not change");
        let held = gear.hands();
        gear.take_in_hand(slot::MAIN_HAND_OPTION_1, 2);
        assert_eq!(gear.hands(), held, "the same weapon is no change");
        // A later draw the own client is told of is a change of the hands alone.
        assert!(gear.draw(1, 0, 3));
        let change = gear.take_change().expect("the client is told");
        assert_eq!(
            change
                .paperdoll
                .owner
                .as_ref()
                .and_then(|owner| owner.visible_items.as_ref()),
            None
        );
        assert_eq!(gear.shown_version(), items, "the version stays");
    }

    #[test]
    fn a_change_of_the_hands_goes_out_as_its_children_alone() {
        /// Retail newchar 2026-09-18 at 1792695769648: the draw of the attack press sent the
        /// own paperdoll's `a2`, `sheathed` and `attached`, not the item lists or the bag.
        const DRAW_NS: u64 = 1_792_695_769_624_369_200;
        let mut gear = Gear::starter(&kits(), CHARACTER, Some(BACKSTORY)).expect("gear");
        let first = gear.first_states();
        assert!(
            first
                .paperdoll
                .owner
                .as_ref()
                .is_some_and(|owner| owner.visible_items.is_some())
        );
        assert_eq!(gear.take_change(), None, "the first states told the hands");
        assert!(gear.draw_to_attack(DRAW_NS));
        let draw = gear.take_change().expect("the client is told");
        let owner = draw.paperdoll.owner.as_ref().expect("owner");
        assert_eq!(
            (
                owner.main_hand_ammo_1,
                owner.main_hand_ammo_2.map(|ammo| ammo.time_ns)
            ),
            (None, Some(DRAW_NS)),
            "the stamped option alone"
        );
        assert_eq!(
            (
                owner.visible_items.as_ref(),
                owner.loadouts.as_ref(),
                owner.is_local_player
            ),
            (None, None, None)
        );
        let public = draw.paperdoll.public.as_ref().expect("public");
        assert!(public.sheathed.is_some() && public.attached.is_some());
        assert_eq!(
            (
                public.active,
                public.visible_slots.as_ref(),
                public.visuals.as_ref()
            ),
            (None, None, None),
            "the selected option did not change; no slot or look list"
        );
        assert_eq!(draw.bag, ContainerState::default(), "no bag");
        assert_eq!(gear.take_change(), None, "once");
        // A change of the items sends its changed children at a new version.
        let version = gear.shown_version();
        assert!(gear.remove(SWORD, DRAW_NS));
        let whole = gear.take_change().expect("change");
        assert!(
            whole
                .paperdoll
                .owner
                .is_some_and(|owner| owner.visible_items.is_some())
        );
        assert!(gear.shown_version() > version);
    }

    #[test]
    fn weapons_swap_slots_and_the_picked_one_is_in_the_hand() {
        const SECOND_OPTION: u32 = slot::MAIN_HAND_OPTION_1 + 1;
        const ANY_BAG_SLOT: u32 = u32::MAX;
        const EMPTY: [u8; RAW_ID_LEN] = [0; RAW_ID_LEN];
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        assert_eq!(
            gear.swap(slot::MAIN_HAND_OPTION_1, SECOND_OPTION, EMPTY, EMPTY),
            Err(EquipRefusal::NotWorn),
            "the ids must match the worn items"
        );
        assert_eq!(
            gear.swap(
                slot::CHEST,
                SECOND_OPTION,
                instance("MysticArchetypeChest"),
                EMPTY
            ),
            Err(EquipRefusal::WrongSlot)
        );
        assert_eq!(
            gear.swap(
                slot::MAIN_HAND_OPTION_1,
                SECOND_OPTION,
                instance(SWORD),
                EMPTY
            ),
            Ok(())
        );
        assert!(gear.take_change().is_some());
        assert_eq!(gear.main_weapon(), Some((SWORD, 75)), "the only weapon");
        gear.draw(0, 0, 0);
        assert_eq!(
            gear.main_weapon(),
            Some((SWORD, 75)),
            "an empty pick keeps one"
        );
        gear.draw(1, 0, 0);
        assert_eq!(gear.main_weapon(), Some((SWORD, 75)));
        assert_eq!(
            gear.unequip(slot::MAIN_HAND_OPTION_1, ANY_BAG_SLOT),
            Err(EquipRefusal::NotWorn)
        );
        assert_eq!(gear.unequip(SECOND_OPTION, ANY_BAG_SLOT), Ok(()));
        assert_eq!(gear.main_weapon(), None);
        assert_eq!(
            gear.bag[0].as_ref().map(|stack| stack.item_id.as_str()),
            Some(SWORD)
        );
    }

    #[test]
    fn an_item_of_a_disabled_class_stays_in_the_bag() {
        let kits = kits();
        let mut gear = Gear::starter(&kits, CHARACTER, Some(BACKSTORY)).expect("gear");
        assert!(gear.add(kits.item(STAFF).expect("staff"), 1));
        let staff = |item_id: &str| item_id == STAFF;
        assert_eq!(
            gear.equip(0, slot::MAIN_HAND_OPTION_1, 1, instance(STAFF), staff),
            Err(EquipRefusal::Disabled)
        );
        assert_eq!(gear.bag_count(STAFF), 1, "it stays in the bag");
        assert_ne!(gear.worn_item(slot::MAIN_HAND_OPTION_1), Some(STAFF));
        assert_eq!(
            gear.equip(
                0,
                slot::MAIN_HAND_OPTION_1,
                1,
                instance(STAFF),
                none_disabled
            ),
            Ok(STAFF.to_owned()),
            "another item class equips"
        );
    }
}
