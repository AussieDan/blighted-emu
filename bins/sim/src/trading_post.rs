// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trading post's rules and its wire form (economy note
//! `local-data/scratch-work/run15/economy/REPORT.md`): the `[trading_post]` section of
//! `config/sim.toml`, the trading post fields of every item (`MasterItemDefinitions`
//! `TradingCategory`, `TradingFamily`, `TradingGroup`, `Tier`, `RequiredLevel`), the posting
//! fee and tax of an order, and an order of the world database as the client's contract.
//!
//! Retail values (Observed): posting durations of 1, 3, 7 and 14 days (every captured order);
//! a trading tax of 5 % at the settlements of three captured characters without a territory
//! standing bonus; at most 40 open orders (`ContractConfigData` default, `FUN_145c173a0`).
//! The retail posting fee table (`ContractConfigData` `Buy/Sell Contract Duration Fee Map`) is
//! in no client pak and its values are Unknown; this server charges the fees the section lists,
//! none by default.

use std::collections::HashMap;

use nw_content::Tables;
use nw_db::{Order, OrderEnd, OrderItem, OrderKind, OrderStatus, SavedItemRolls};
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::contracts::{
    CONTRACT_ENTITY_IDS, Contract, ContractItemData, ContractKind, INVALID_ENTITY_ID, Participant,
    ParticipantEvent, reason, status,
};
use nw_messages::item_descriptor::ItemDescriptor;
use nw_world::records::Capital;
use serde::Deserialize;
use uuid::Uuid;

use crate::config::SimConfigError;
use crate::travel_rules::capital_id;

/// Nanoseconds per millisecond: contract times are Unix nanoseconds.
const NS_PER_MS: u64 = 1_000_000;
/// Milliseconds per day: posting durations are whole days.
pub const MS_PER_DAY: u64 = 86_400_000;
/// The posting durations retail offers, in days.
pub const RETAIL_DURATIONS_DAYS: [u32; 4] = [1, 3, 7, 14];
/// Most open orders of one character (`ContractConfigData` `Base Number of Buy/Sell
/// Contracts`, constructor default).
pub const RETAIL_MAX_OPEN_ORDERS: u32 = 40;
/// The trading tax of a settlement without its own (Observed: 5 % at the settlements of three
/// retail characters without a standing bonus).
pub const RETAIL_TRADING_TAX: f64 = 0.05;
/// Default time between two takes of the deliveries of a sim's players (engineering: a sale
/// on another sim shows within a few seconds).
pub const DEFAULT_DELIVERY_POLL_MS: u64 = 2_000;
/// The wire required level of an item without one.
const NO_REQUIRED_LEVEL: u32 = u32::MAX;
/// The `BindOnPickup` value of an item that binds when taken: such an item cannot be traded.
const BINDS_ON_PICKUP: i64 = 1;

/// Which orders a posting fee is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeKind {
    /// Buy orders.
    Buy,
    /// Sell orders.
    Sell,
}

/// The posting fee of one duration: a flat part and a share of the order's total.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostingFee {
    /// Buy or sell orders.
    pub kind: FeeKind,
    /// The duration, in days.
    pub days: u32,
    /// The flat part, hundredths of a coin.
    pub flat: u64,
    /// The share of the total, 0 to below 1.
    pub share: f64,
}

/// A settlement with its own trading tax.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutpostTax {
    /// The outpost id.
    pub outpost: u64,
    /// The tax, 0 to below 1.
    pub trading_tax: f64,
}

/// `[trading_post]` of `config/sim.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TradingPostConfig {
    /// The posting durations a player may pick, in days.
    pub durations_days: Vec<u32>,
    /// Most open orders of one character.
    pub max_open_orders: u32,
    /// The trading tax of a settlement without its own.
    pub trading_tax: f64,
    /// Settlements with their own tax.
    pub outposts: Vec<OutpostTax>,
    /// The posting fees; a duration without one costs nothing to post.
    pub posting_fees: Vec<PostingFee>,
    /// Time between two takes of the deliveries of this sim's players.
    pub delivery_poll_ms: u64,
}

impl Default for TradingPostConfig {
    fn default() -> Self {
        Self {
            durations_days: RETAIL_DURATIONS_DAYS.to_vec(),
            max_open_orders: RETAIL_MAX_OPEN_ORDERS,
            trading_tax: RETAIL_TRADING_TAX,
            outposts: Vec::new(),
            posting_fees: Vec::new(),
            delivery_poll_ms: DEFAULT_DELIVERY_POLL_MS,
        }
    }
}

impl TradingPostConfig {
    /// Checks the section.
    ///
    /// # Errors
    ///
    /// Returns [`SimConfigError::Invalid`] for a rate outside 0 to below 1, no duration, a zero
    /// duration, no open order allowed or a zero poll interval.
    pub fn validate(&self) -> Result<(), SimConfigError> {
        let invalid = |key, reason| Err(SimConfigError::Invalid { key, reason });
        let rate = |value: f64| value.is_finite() && (0.0..1.0).contains(&value);
        if self.durations_days.is_empty() || self.durations_days.contains(&0) {
            return invalid(
                "trading_post.durations_days",
                "must list at least one duration, each above 0",
            );
        }
        if self.max_open_orders == 0 {
            return invalid("trading_post.max_open_orders", "must be above 0");
        }
        if !rate(self.trading_tax) || self.outposts.iter().any(|tax| !rate(tax.trading_tax)) {
            return invalid("trading_post.trading_tax", "must be at least 0 and below 1");
        }
        if self.posting_fees.iter().any(|fee| !rate(fee.share)) {
            return invalid(
                "trading_post.posting_fees",
                "shares must be at least 0 and below 1",
            );
        }
        if self.delivery_poll_ms == 0 {
            return invalid("trading_post.delivery_poll_ms", "must be above 0");
        }
        Ok(())
    }

    /// Whether a player may post for `days`.
    pub fn allows(&self, days: u32) -> bool {
        self.durations_days.contains(&days)
    }

    /// The trading tax of `outpost`.
    pub fn tax_at(&self, outpost: u64) -> f64 {
        self.outposts
            .iter()
            .find(|tax| tax.outpost == outpost)
            .map_or(self.trading_tax, |tax| tax.trading_tax)
    }

    /// The posting fee of an order of `kind` for `days` worth `total`, hundredths; `None` when
    /// it does not fit the coin range.
    pub fn posting_fee(&self, kind: OrderKind, days: u32, total: u64) -> Option<u64> {
        let wanted = match kind {
            OrderKind::Buy => FeeKind::Buy,
            OrderKind::Sell => FeeKind::Sell,
        };
        let Some(fee) = self
            .posting_fees
            .iter()
            .find(|fee| fee.kind == wanted && fee.days == days)
        else {
            return Some(0);
        };
        fee.flat.checked_add(nw_db::tax_of(total, fee.share))
    }
}

/// The trading post fields of one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeItem {
    /// `ItemID`.
    pub item_id: String,
    /// `TradingCategory`.
    pub category: String,
    /// `TradingFamily`.
    pub family: String,
    /// `TradingGroup`.
    pub group: String,
    /// `Tier`.
    pub tier: u32,
    /// `RequiredLevel`; `None` for none.
    pub required_level: Option<u32>,
}

impl TradeItem {
    /// The order item of a stack of this item at `gear_score`, with the CRC-32 of each of its
    /// lowercase perk ids `perks`, `gem_perks` of them gems (`PerkType` `Gem`), and the stack's
    /// own values `rolls` (a buy order's are the data values).
    pub fn order_item(
        &self,
        gear_score: u32,
        perks: Vec<u32>,
        gem_perks: u32,
        rolls: SavedItemRolls,
    ) -> OrderItem {
        OrderItem {
            item_id: self.item_id.clone(),
            item_crc: lowercase_crc32(&self.item_id),
            category: self.category.clone(),
            family: self.family.clone(),
            group: self.group.clone(),
            tier: self.tier,
            gear_score,
            required_level: self.required_level,
            perks,
            gem_perk_count: gem_perks,
            rolls,
        }
    }
}

/// The items that may be traded, by the CRC-32 of their lowercase id: those with a trading
/// category that do not bind when taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TradingCatalog {
    items: HashMap<u32, TradeItem>,
}

impl TradingCatalog {
    /// The tradable items of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        let items = tables
            .master_item_definitions
            .iter()
            .flat_map(|table| &table.rows)
            .filter(|row| row.bind_on_pickup != Some(BINDS_ON_PICKUP))
            .filter_map(|row| {
                let category = row
                    .trading_category
                    .clone()
                    .filter(|text| !text.is_empty())?;
                Some((
                    lowercase_crc32(&row.item_id),
                    TradeItem {
                        item_id: row.item_id.clone(),
                        category,
                        family: row.trading_family.clone().unwrap_or_default(),
                        group: row.trading_group.clone().unwrap_or_default(),
                        tier: row
                            .tier
                            .and_then(|tier| u32::try_from(tier).ok())
                            .unwrap_or_default(),
                        required_level: row
                            .required_level
                            .and_then(|level| u32::try_from(level).ok()),
                    },
                ))
            })
            .collect();
        Self { items }
    }

    /// The catalog of `items`.
    #[cfg(test)]
    pub fn of(items: impl IntoIterator<Item = TradeItem>) -> Self {
        Self {
            items: items
                .into_iter()
                .map(|item| (lowercase_crc32(&item.item_id), item))
                .collect(),
        }
    }

    /// How many items may be traded.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether no item may be traded (content without item definitions).
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The tradable item whose lowercase id has the CRC-32 `crc`.
    pub fn by_crc(&self, crc: u32) -> Option<&TradeItem> {
        self.items.get(&crc)
    }

    /// The tradable item `item_id` (any case).
    pub fn by_id(&self, item_id: &str) -> Option<&TradeItem> {
        self.by_crc(lowercase_crc32(item_id))
    }
}

/// The slice folder of the settlements' capitals.
const SETTLEMENT_FOLDER: &str = "structures/settlements/v2_settlements/";
/// The slice name end of a settlement's capital.
const SETTLEMENT_SUFFIX: &str = "_complete";
/// The slice file name start of an outpost's capital.
const OUTPOST_PREFIX: &str = "outpost_";
/// Outpost slices that hold no trading post or storage: watchtower camps and the Outpost Rush
/// layouts.
const NOT_SETTLEMENT_PREFIXES: [&str; 2] = ["outpost_watchtower", "outpostrush"];
/// The separators of a slice path.
const SLICE_PATH_SEPARATORS: [char; 2] = ['/', '\\'];

/// Whether `slice` (a capital's slice path) is a settlement's or an outpost's: the capitals whose
/// entity ids the trading post and the storage name as their settlement (retail, 22 of 22 ids of
/// the 5 trading post and storage captures: 13 `Structures/Settlements/V2_Settlements/*_Complete`,
/// 7 `POI/Outpost/Outpost_*`, 2 generated `Outpost_FirstLight_*`).
pub fn is_settlement_slice(slice: &str) -> bool {
    let path = slice.to_ascii_lowercase();
    let file = path
        .rsplit(SLICE_PATH_SEPARATORS)
        .next()
        .unwrap_or_default();
    let settlement = path.contains(SETTLEMENT_FOLDER) && path.ends_with(SETTLEMENT_SUFFIX);
    let outpost = file.starts_with(OUTPOST_PREFIX)
        && !NOT_SETTLEMENT_PREFIXES
            .iter()
            .any(|prefix| file.starts_with(prefix));
    settlement || outpost
}

/// The wire form of a settlement id: the capital's entity id ([`capital_id`]) as the `u64` of
/// its little-endian bytes read big-endian. Retail: contract `outpost` `b325909b3cc6434f` is the
/// Ebonscale Reach capital `0x4f43c63c9b9025b3`, whose bytes the client also sends in its storage
/// calls (387) and in a search's settlement filter (`B325909B3CC6434F0000000000000000`).
pub const fn settlement_wire_id(capital: u64) -> u64 {
    u64::from_be_bytes(capital.to_le_bytes())
}

/// The storage of the settlement of wire id `outpost`: the id's bytes and 8 zero bytes, as the
/// client names the storage it uses (retail `storage_open_deposit_withdraw_transfer` 14.641 s:
/// the storage interaction's target `6bbf90ec53d9f35f` and 8 zero bytes, the Brimstone outpost).
pub fn settlement_storage(outpost: u64) -> [u8; nw_messages::wire::RAW_ID_LEN] {
    let mut storage = [0; nw_messages::wire::RAW_ID_LEN];
    storage[..size_of::<u64>()].copy_from_slice(&outpost.to_be_bytes());
    storage
}

/// How far from a settlement's capital its trading post and storage may be used, metres
/// (Inferred: retail uses lay 10 to 46 m from it, 4 of 4; a settlement's radius is Unknown).
pub const SETTLEMENT_REACH_M: f32 = 100.0;

/// A settlement or outpost of the level: its wire id and its capital's place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settlement {
    /// The wire id ([`settlement_wire_id`]).
    pub id: u64,
    /// East.
    pub x: f32,
    /// North.
    pub y: f32,
}

/// The settlements and outposts of the level, which the trading post and the storage name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settlements {
    all: Vec<Settlement>,
}

impl Settlements {
    /// The settlements of the level's `capitals` ([`is_settlement_slice`]).
    pub fn from_capitals(capitals: &[Capital]) -> Self {
        Self {
            all: capitals
                .iter()
                .filter(|capital| is_settlement_slice(&capital.slice_name))
                .filter_map(|capital| {
                    Some(Settlement {
                        id: settlement_wire_id(capital_id(&capital.id)?),
                        x: capital.position[0],
                        y: capital.position[1],
                    })
                })
                .collect(),
        }
    }

    /// The settlements of `all`.
    #[cfg(test)]
    pub fn of(all: Vec<Settlement>) -> Self {
        Self { all }
    }

    /// Whether `outpost` is the wire id of a settlement of the level (an order posted before
    /// settlements were named carries a territory number instead).
    pub fn contains(&self, outpost: u64) -> bool {
        self.all.iter().any(|settlement| settlement.id == outpost)
    }

    /// The wire id of the settlement whose capital is nearest to `(x, y)`, within
    /// [`SETTLEMENT_REACH_M`]; `None` far from every one. Retail: a trading post's orders and a
    /// used storage name the nearest settlement's capital (the player 21 m from the Morrowtide
    /// capital, 46 m from Ebonscale Reach, 15 m from North Windsward, 10 m from the Brimstone
    /// outpost; 4 of 4).
    pub fn nearest(&self, x: f32, y: f32) -> Option<u64> {
        let distance = |s: &Settlement| (s.x - x).hypot(s.y - y);
        self.all
            .iter()
            .filter(|settlement| distance(settlement) <= SETTLEMENT_REACH_M)
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            .map(|settlement| settlement.id)
    }
}

/// The contract kind of an order kind.
pub const fn contract_kind(kind: OrderKind) -> ContractKind {
    match kind {
        OrderKind::Buy => ContractKind::Buy,
        OrderKind::Sell => ContractKind::Sell,
    }
}

/// The order kind of a contract kind.
pub const fn order_kind(kind: ContractKind) -> OrderKind {
    match kind {
        ContractKind::Buy => OrderKind::Buy,
        ContractKind::Sell => OrderKind::Sell,
    }
}

const fn status_value(status: OrderStatus) -> u32 {
    match status {
        OrderStatus::Available => status::AVAILABLE,
        OrderStatus::Completed => status::COMPLETED,
        OrderStatus::Resolved => status::RESOLVED,
    }
}

fn ns(ms: u64) -> u64 {
    ms.saturating_mul(NS_PER_MS)
}

/// Where a contract is shown, which decides some of its fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractView {
    /// A search answer (2538) or a character's orders (2278): the order as stored.
    Listed,
    /// A lookup by id (223).
    LookedUp,
    /// A notice of a change (634, 1117, 2965, 2964, 917): the order as the change left it, with
    /// the version and completion time it had before the change.
    Notice,
}

/// The event's completion count of `order` for `viewer` in `view`. Retail (the 4 trading post
/// captures of the storage and trading post note, 2026-11-01): the units open in each of 6,152
/// searched contracts and in each open, canceled or expired order of its creator's lists; the
/// units done in a fully completed one (2278, 2964, 1117); 0 in each lookup by id of another
/// player's order (13 of 13). After a partial completion the completer's 1117 carried 100 (1 of
/// 50 bought) and 10,000 (72 of 8,226 sold), which no rule found explains (Unknown); this server
/// sends the units open there (Inferred, as a search).
fn completion_count(order: &Order, mine: bool, view: ContractView) -> u32 {
    if view == ContractView::LookedUp && !mine {
        0
    } else if order.status != OrderStatus::Available && order.end == OrderEnd::Succeeded {
        order.completed_units()
    } else {
        order.open
    }
}

/// `order` as the contract that character `viewer` sees in `view`. Retail (same captures):
/// the event's reason is the order's end once it is closed (status 3 or 4), for its creator and
/// for its completer alike, and none while it is open (every searched and listed open order);
/// the completion time is set only for a completed order, never for a canceled or expired one
/// (9 of 9 listed); a notice carries the version before its change (634 v0 then lists v1, a
/// completion's 1117 and 2964 v1 after lists v1 then lists v2, the proceeds' 1117 v2 then lists
/// v3, a cancel's 2965 v1 then lists v2) and no completion time for the change that completed
/// the order (the completer's 1117 and the creator's 2964, done time 0; the lists after it carry
/// the time).
pub fn contract_of(order: &Order, viewer: Uuid, view: ContractView) -> Contract {
    let mine = order.creator == viewer;
    let closed = order.status != OrderStatus::Available;
    let event_reason = if closed {
        order.end.value()
    } else {
        OrderEnd::None.value()
    };
    let notice = view == ContractView::Notice;
    let completed_ns = match order.completed_ms {
        Some(ms)
            if order.end == OrderEnd::Succeeded
                && !(notice && order.status == OrderStatus::Completed) =>
        {
            ns(ms)
        }
        _ => 0,
    };
    let version = if notice {
        order.version.saturating_sub(1)
    } else {
        order.version
    };
    let perk_count = u32::try_from(order.item.perks.len()).unwrap_or(u32::MAX);
    let gear_score = u16::try_from(order.item.gear_score).unwrap_or(u16::MAX);
    Contract {
        kind: contract_kind(order.kind),
        id: *order.id.as_bytes(),
        version,
        status: status_value(order.status),
        created_ns: ns(order.created_ms),
        completed_ns,
        updated_ns: ns(order.updated_ms),
        duration_ns: ns(order.expires_ms.saturating_sub(order.created_ms)),
        visibility: Participant::NONE,
        local_creator: mine,
        events: vec![(
            Participant::NONE,
            ParticipantEvent::of(event_reason, completion_count(order, mine, view)),
        )],
        item: ContractItemData {
            category: order.item.category.as_bytes().to_vec(),
            family: order.item.family.as_bytes().to_vec(),
            group: order.item.group.as_bytes().to_vec(),
            name: order.item.item_id.as_bytes().to_vec(),
            gear_score: order.item.gear_score,
            tier: order.item.tier,
            perks: order.item.perks.clone(),
            perk_attributes: Vec::new(),
            rarity_level: 0,
            required_level: order.item.required_level.unwrap_or(NO_REQUIRED_LEVEL),
            perk_count,
            gem_perk_count: order.item.gem_perk_count,
            instance: [0; nw_messages::wire::RAW_ID_LEN],
        },
        entity_ids: [INVALID_ENTITY_ID; CONTRACT_ENTITY_IDS],
        invalid_participant: Participant::NONE,
        invalid_event: ParticipantEvent::invalid(),
        invalid_reason: reason::NONE,
        outpost: order.outpost,
        reserved: 0,
        price: order.price,
        quantity: order.open,
        sold_item: (order.kind == OrderKind::Sell).then(|| ItemDescriptor {
            gear_score,
            perks: order.item.perks.clone(),
            ..ItemDescriptor::of(order.item.item_crc, order.open)
        }),
    }
}

/// The next-page token of a search answer after `last` (retail: base64 of
/// `[price, expiry ms, "cn-<contract id>"]`).
pub fn next_page_token(last: &Order) -> Vec<u8> {
    let text = format!(
        "[{}, {}, \"cn-{}\"]",
        last.price,
        last.expires_ms,
        last.id.simple().to_string().to_uppercase()
    );
    data_encoding::BASE64.encode(text.as_bytes()).into_bytes()
}

/// The coins and items of `units` of order `order` that a completer of trading tax
/// `completer_rate` pays and brings: a sell order's buyer pays the total and its tax
/// ([`nw_db::tax_of`] at its own rate, as [`nw_db::complete_order`]); a buy order's seller
/// brings the units.
pub fn completion_cost(
    order: &Order,
    units: u32,
    completer_rate: f64,
) -> Option<(u64, Option<(String, u32)>)> {
    let total = order.price.checked_mul(u64::from(units))?;
    Some(match order.kind {
        OrderKind::Sell => (
            total.checked_add(nw_db::tax_of(total, completer_rate))?,
            None,
        ),
        OrderKind::Buy => (0, Some((order.item.item_id.clone(), units))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nw_messages::wire::{Reader, Writer};

    const NOW_MS: u64 = 1_781_457_196_968;
    /// The trading tax of the retail `buy-sell-TP` character (a standing bonus).
    const STANDING_TRADING_TAX: f64 = 0.0235;

    fn beeswax() -> TradeItem {
        TradeItem {
            item_id: "BeeswaxT1".to_owned(),
            category: "Resources".to_owned(),
            family: "RawResources".to_owned(),
            group: "Beeswax".to_owned(),
            tier: 1,
            required_level: None,
        }
    }

    fn order(kind: OrderKind, creator: Uuid) -> Order {
        Order {
            id: Uuid::from_u128(0x8233_f5b0_d351_42a3_b433_1068_4f88_a957),
            kind,
            status: OrderStatus::Available,
            version: 1,
            creator,
            outpost: 0xb325_909b_3cc6_434f,
            item: beeswax().order_item(0, Vec::new(), 0, SavedItemRolls::default()),
            price: 10_000,
            posted: 1,
            open: 1,
            end: OrderEnd::None,
            tax_rate: RETAIL_TRADING_TAX,
            created_ms: NOW_MS,
            expires_ms: NOW_MS + 3 * MS_PER_DAY,
            completed_ms: None,
            updated_ms: NOW_MS,
        }
    }

    /// The retail one-result search answer's buy order (`buy-sell-TP` 358458 ms), as another
    /// player sees it: the same fields this server sends, from the item data and the order.
    #[test]
    fn an_order_is_the_retail_contract_form() {
        let contract = contract_of(
            &order(OrderKind::Buy, Uuid::from_u128(1)),
            Uuid::from_u128(2),
            ContractView::Listed,
        );
        assert_eq!(contract.kind, ContractKind::Buy);
        assert!(!contract.local_creator);
        assert_eq!(contract.duration_ns, 3 * MS_PER_DAY * NS_PER_MS);
        assert_eq!(contract.item.category, b"Resources");
        assert_eq!(contract.item.required_level, NO_REQUIRED_LEVEL);
        assert_eq!(contract.events[0].1.reason, reason::NONE);
        assert_eq!(contract.events[0].1.completion_count, 1);
        assert!(contract.sold_item.is_none());
        let mut writer = Writer::new();
        contract.write(&mut writer).expect("write");
        let bytes = writer.finish();
        assert_eq!(Contract::read(&mut Reader::new(&bytes)), Ok(contract));
    }

    #[test]
    fn a_sell_order_carries_its_units_and_its_creator_sees_its_end() {
        let creator = Uuid::from_u128(1);
        let mut sold = order(OrderKind::Sell, creator);
        sold.end = OrderEnd::Canceled;
        sold.status = OrderStatus::Resolved;
        let contract = contract_of(&sold, creator, ContractView::Listed);
        assert!(contract.local_creator);
        assert_eq!(contract.status, status::RESOLVED);
        assert_eq!(contract.events[0].1.reason, reason::CANCELED);
        let item = contract.sold_item.expect("a sell order's item");
        assert_eq!((item.item_crc, item.quantity), (0xd0a8_26e7, 1));
    }

    /// The owner's case on the wire: 10 listed, 1 bought. Retail: the units open in searches
    /// and in the creator's list (`buy-sell-TP` 214.948 s), no reason while open, 0 units in a
    /// lookup of another player's order (219.826 s) and the version before the change in the
    /// completer's 1117 (`tradepost_post_sell_cancel_buyorder_sellorder` 22.292 s: v2, status 0,
    /// 49 of 50 left).
    #[test]
    fn a_partly_bought_order_shows_the_units_left_everywhere() {
        let seller = Uuid::from_u128(1);
        let buyer = Uuid::from_u128(2);
        let mut listed = order(OrderKind::Sell, seller);
        listed.posted = 10;
        listed.open = 9;
        listed.version = 3;
        listed.end = OrderEnd::Succeeded;
        for (viewer, view) in [
            (buyer, ContractView::Listed),
            (seller, ContractView::Listed),
            (buyer, ContractView::Notice),
        ] {
            let contract = contract_of(&listed, viewer, view);
            assert_eq!(contract.quantity, 9);
            assert_eq!(contract.sold_item.map(|item| item.quantity), Some(9));
            assert_eq!(contract.events[0].1.completion_count, 9);
            assert_eq!(contract.events[0].1.reason, reason::NONE);
        }
        assert_eq!(
            contract_of(&listed, buyer, ContractView::LookedUp).events[0]
                .1
                .completion_count,
            0
        );
        assert_eq!(contract_of(&listed, buyer, ContractView::Notice).version, 2);
        assert_eq!(contract_of(&listed, buyer, ContractView::Listed).version, 3);
    }

    /// The rest bought: retail `buy-sell-TP` 222.892 s (the completer's 1117: status 3, reason 1,
    /// the units done, no completion time, the version before) and 248.487 s (the creator's list
    /// after the proceeds: status 4 with the completion time).
    #[test]
    fn a_fully_bought_order_shows_its_units_done_and_its_end() {
        let seller = Uuid::from_u128(1);
        let buyer = Uuid::from_u128(2);
        let mut done = order(OrderKind::Sell, seller);
        done.posted = 10;
        done.open = 0;
        done.version = 4;
        done.end = OrderEnd::Succeeded;
        done.status = OrderStatus::Completed;
        done.completed_ms = Some(NOW_MS);
        let notice = contract_of(&done, buyer, ContractView::Notice);
        assert_eq!(notice.status, status::COMPLETED);
        assert_eq!(notice.events[0].1.reason, reason::SUCCEEDED);
        assert_eq!(notice.events[0].1.completion_count, 10);
        assert_eq!(notice.completed_ns, 0);
        assert_eq!(notice.version, 3);
        assert_eq!(notice.quantity, 0);
        done.status = OrderStatus::Resolved;
        let listed = contract_of(&done, seller, ContractView::Listed);
        assert_eq!(listed.completed_ns, NOW_MS * NS_PER_MS);
        assert_eq!(listed.events[0].1.completion_count, 10);
        let mut canceled = done.clone();
        canceled.end = OrderEnd::Canceled;
        canceled.open = 3;
        let listed = contract_of(&canceled, seller, ContractView::Listed);
        assert_eq!(
            listed.completed_ns, 0,
            "retail: no time on a canceled order"
        );
        assert_eq!(listed.events[0].1.completion_count, 3);
    }

    #[test]
    fn the_retail_rules_are_the_defaults() {
        let config = TradingPostConfig::default();
        assert!(config.validate().is_ok());
        assert!(config.allows(14) && !config.allows(2));
        assert_eq!(config.tax_at(1), RETAIL_TRADING_TAX);
        assert_eq!(config.posting_fee(OrderKind::Sell, 3, 5_250), Some(0));
        let priced = TradingPostConfig {
            posting_fees: vec![PostingFee {
                kind: FeeKind::Sell,
                days: 3,
                flat: 100,
                share: 0.02,
            }],
            outposts: vec![OutpostTax {
                outpost: 7,
                trading_tax: 0.0235,
            }],
            ..TradingPostConfig::default()
        };
        assert_eq!(priced.posting_fee(OrderKind::Sell, 3, 5_250), Some(205));
        assert_eq!(priced.posting_fee(OrderKind::Buy, 3, 5_250), Some(0));
        assert_eq!(priced.tax_at(7), 0.0235);
        let broken = TradingPostConfig {
            trading_tax: 1.0,
            ..TradingPostConfig::default()
        };
        assert!(broken.validate().is_err());
    }

    #[test]
    fn a_buyer_pays_the_total_and_its_tax() {
        let sell = order(OrderKind::Sell, Uuid::from_u128(1));
        assert_eq!(
            completion_cost(&sell, 1, RETAIL_TRADING_TAX),
            Some((10_500, None))
        );
        // Retail `buy-sell-TP` 221.764 s: 2 at 10,000 cost a buyer of rate 2.35 % 20,470.
        assert_eq!(
            completion_cost(&sell, 2, STANDING_TRADING_TAX),
            Some((20_470, None))
        );
        let buy = order(OrderKind::Buy, Uuid::from_u128(1));
        assert_eq!(
            completion_cost(&buy, 1, RETAIL_TRADING_TAX),
            Some((0, Some(("BeeswaxT1".to_owned(), 1))))
        );
    }

    #[test]
    fn the_page_token_has_the_retail_form() {
        let token = next_page_token(&order(OrderKind::Sell, Uuid::from_u128(1)));
        let text = data_encoding::BASE64.decode(&token).expect("base64");
        assert_eq!(
            String::from_utf8(text).expect("text"),
            format!(
                "[10000, {}, \"cn-8233F5B0D35142A3B43310684F88A957\"]",
                NOW_MS + 3 * MS_PER_DAY
            )
        );
    }

    /// A capital of the level at `(x, y)`.
    fn capital(guid: &str, x: f32, y: f32, slice: &str) -> Capital {
        Capital {
            id: guid.to_owned(),
            position: [x, y, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: None,
            slice_name: slice.to_owned(),
            variant_name: None,
            slice_asset_id: String::new(),
        }
    }

    /// The retail settlement ids and the capitals of the level file that make them (outposts
    /// probe of the storage and trading post note, 2026-11-01): Ebonscale Reach is the outpost
    /// of the `buy-sell-TP` and `Hivesellandbuyorder` orders, Morrowtide of the orders of
    /// `tradepost_post_sell_cancel_buyorder_sellorder`, the Brimstone outpost the storage of
    /// `storage_open_deposit_withdraw_transfer`; each player stood nearest to that capital.
    #[test]
    fn a_trading_post_names_the_nearest_settlements_retail_id() {
        const EBONSCALE: u64 = 0xb325_909b_3cc6_434f;
        const MORROWTIDE: u64 = 0x63e5_ce00_1ef4_6668;
        const BRIMSTONE_OUTPOST: u64 = 0x6bbf_90ec_53d9_f35f;
        let settlements = Settlements::from_capitals(&[
            capital(
                "1883e77a-123e-9781-57c0-214689aeb235",
                7292.633,
                5374.028,
                "Structures/Settlements/V2_Settlements/Ebonscale_Reach_Settlement_Complete",
            ),
            capital(
                "f0359664-e376-3f5e-9853-627ae3b8da3d",
                13177.792,
                6974.558,
                "Structures/Settlements/V2_Settlements/Settlement_Morrowtide_Complete",
            ),
            capital(
                "4590ff67-957f-3867-1a63-263479ef870b",
                7731.22,
                7203.719,
                "POI/Outpost/Outpost_Brimstone_A",
            ),
            capital(
                "7df376b2-a316-3687-bd77-5b459cf1192a",
                7300.0,
                5380.0,
                "Structures/Settlements/V2_Settlements/Settlement_Station_Kiln_Master",
            ),
        ]);
        assert_eq!(settlements.nearest(7252.04, 5396.73), Some(EBONSCALE));
        assert_eq!(settlements.nearest(13158.09, 6966.27), Some(MORROWTIDE));
        assert_eq!(
            settlements.nearest(7727.03, 7193.73),
            Some(BRIMSTONE_OUTPOST)
        );
        assert!(settlements.contains(EBONSCALE) && !settlements.contains(2));
        assert_eq!(Settlements::default().nearest(0.0, 0.0), None);
        assert_eq!(
            settlements.nearest(7252.04 + SETTLEMENT_REACH_M * 2.0, 5396.73),
            None,
            "far from every settlement"
        );
        assert_eq!(settlement_wire_id(0x4f43_c63c_9b90_25b3), EBONSCALE);
        let mut storage = [0; nw_messages::wire::RAW_ID_LEN];
        storage[..8].copy_from_slice(&[0x6b, 0xbf, 0x90, 0xec, 0x53, 0xd9, 0xf3, 0x5f]);
        assert_eq!(settlement_storage(BRIMSTONE_OUTPOST), storage);
    }

    #[test]
    fn settlement_and_outpost_capitals_are_told_from_the_rest() {
        for slice in [
            "Structures/Settlements/V2_Settlements/Shanty_WeaversFen_Complete",
            "POI/Outpost/Outpost_T3_B_Sanctuary_VLG",
            "CoatGen\\1d6a7490\\Outpost_FirstLight_A_491652771.dynamicslice",
        ] {
            assert!(is_settlement_slice(slice), "{slice}");
        }
        for slice in [
            "Structures/Settlements/V2_Settlements/Settlement_Station_Kiln_Master",
            "CoatGen\\x\\Outpost_Watchtower_C_06_Windsward_1.dynamicslice",
            "POIs/OutpostRush/OutpostRush_Layout_00",
            "Zone_CapitalPlot_NoSettlement_Master",
        ] {
            assert!(!is_settlement_slice(slice), "{slice}");
        }
    }

    #[test]
    fn the_catalog_finds_an_item_by_its_crc() {
        let catalog = TradingCatalog::of([beeswax()]);
        assert_eq!(
            catalog
                .by_crc(0xd0a8_26e7)
                .map(|item| item.item_id.as_str()),
            Some("BeeswaxT1")
        );
        assert!(catalog.by_id("beeswaxt1").is_some());
        assert!(catalog.by_crc(1).is_none());
    }
}
