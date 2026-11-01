// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trading post of a world (world migration 0016): buy and sell orders, what the trading
//! post owes each character, and the economy audit log. Every sim of the world and the world
//! director reach the orders here, so an order posted on one sim is found, completed, canceled
//! and expired from any other (architecture spec sections 4.6 and 7.1).
//!
//! Each value transfer is one `SERIALIZABLE` transaction with its audit rows, retried on a
//! serialization failure. The sim takes the coins and items a player gives before it calls a
//! transfer and gives them back when the transfer is refused; what a player receives waits here
//! as a delivery until its sim takes it ([`take_deliveries`]).
//!
//! The tax rules are the retail ones of the captured trades (economy note, section 3, and the
//! storage and trading post note of 2026-11-01): the buyer of a sell order pays the price and the
//! tax on it at the completer's rate; the seller of a sell order gets the price less the tax at
//! the order's rate; the seller to a buy order gets the price less the tax at the completer's
//! rate; the creator of a buy order pays the price and the posting fee when it posts, and nothing
//! at its completion. Each tax is the price times the rate, rounded down ([`tax_of`]).

use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbError;

/// SQLSTATE of a serialization failure.
const SERIALIZATION_FAILURE: &str = "40001";
/// Tries of one transfer before a serialization failure is the caller's.
const TRANSFER_ATTEMPTS: u32 = 5;
/// Milliseconds per second, for the stored times.
const MILLIS_PER_SECOND: f64 = 1000.0;
/// Most matches a search counts (retail answers report at most 10,000).
pub const MAX_COUNTED_MATCHES: i64 = 10_000;
/// The stored required level of an item without one (`-1` on the wire).
const NO_REQUIRED_LEVEL: i32 = -1;
/// Audit kinds.
const AUDIT_POST: &str = "trading_post_post";
const AUDIT_BUY: &str = "trading_post_buy";
const AUDIT_SELL: &str = "trading_post_sell";
const AUDIT_CANCEL: &str = "trading_post_cancel";
const AUDIT_EXPIRE: &str = "trading_post_expire";
const AUDIT_DELIVERED: &str = "trading_post_delivered";
const AUDIT_TRADE: &str = "player_trade";

/// The columns of an order row, with its times as Unix milliseconds.
macro_rules! order_columns {
    () => {
        "contract_id, kind, status, version, creator_id, outpost, item_id, item_crc, category, \
         family, item_group, tier, gear_score, required_level, perks, gem_perk_count, price, \
         quantity_posted, quantity_open, reason, tax_rate, \
         (EXTRACT(EPOCH FROM created_at) * 1000)::BIGINT AS created_ms, \
         (EXTRACT(EPOCH FROM expires_at) * 1000)::BIGINT AS expires_ms, \
         (EXTRACT(EPOCH FROM completed_at) * 1000)::BIGINT AS completed_ms, \
         (EXTRACT(EPOCH FROM updated_at) * 1000)::BIGINT AS updated_ms"
    };
}

/// The filter of a search: parameters 1 to 10 ([`bind_search`]).
macro_rules! search_filter {
    () => {
        "status = $1 AND kind = $2 AND expires_at > to_timestamp($3) \
         AND ($4::TEXT IS NULL OR category = $4) AND ($5::TEXT IS NULL OR family = $5) \
         AND ($6::TEXT IS NULL OR item_group = $6) AND ($7::TEXT IS NULL OR item_id = $7) \
         AND ($8::INTEGER IS NULL OR tier = $8) \
         AND (cardinality($9::BIGINT[]) = 0 OR outpost = ANY($9)) AND perks @> $10::JSONB"
    };
}

/// Binds parameters 1 to 10 of [`search_filter`] for `$search` to `$query`.
macro_rules! bind_search {
    ($query:expr, $search:expr, $outposts:expr, $tier:expr) => {
        $query
            .bind(STATUS_AVAILABLE)
            .bind($search.kind.column())
            .bind(seconds($search.now_ms))
            .bind($search.category.as_deref())
            .bind($search.family.as_deref())
            .bind($search.group.as_deref())
            .bind($search.item_id.as_deref())
            .bind($tier)
            .bind($outposts.clone())
            .bind(Json($search.perks.clone()))
    };
}

/// The tax on `total` at `rate`, rounded down (retail: 470 on 20,000 at 2.35 %, 49 on 992 at
/// 5 %).
pub fn tax_of(total: u64, rate: f64) -> u64 {
    // A rate is below 1, so the tax fits below `total`; float rounding down as retail.
    let tax = (total as f64 * rate).floor();
    if tax.is_finite() && tax > 0.0 {
        (tax as u64).min(total)
    } else {
        0
    }
}

/// A buy or sell order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderKind {
    /// The creator pays now; sellers bring the items.
    Buy,
    /// The creator listed the items; buyers pay.
    Sell,
}

impl OrderKind {
    const fn column(self) -> i16 {
        match self {
            Self::Buy => 0,
            Self::Sell => 1,
        }
    }

    const fn of_column(column: i16) -> Option<Self> {
        match column {
            0 => Some(Self::Buy),
            1 => Some(Self::Sell),
            _ => None,
        }
    }
}

/// Where an order is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    /// Open for completions.
    Available,
    /// Every unit is done; the creator's proceeds wait.
    Completed,
    /// Closed.
    Resolved,
}

impl OrderStatus {
    const fn column(self) -> i16 {
        match self {
            Self::Available => STATUS_AVAILABLE,
            Self::Completed => STATUS_COMPLETED,
            Self::Resolved => STATUS_RESOLVED,
        }
    }

    const fn of_column(column: i16) -> Option<Self> {
        match column {
            STATUS_AVAILABLE => Some(Self::Available),
            STATUS_COMPLETED => Some(Self::Completed),
            STATUS_RESOLVED => Some(Self::Resolved),
            _ => None,
        }
    }
}

const STATUS_AVAILABLE: i16 = 0;
const STATUS_COMPLETED: i16 = 3;
const STATUS_RESOLVED: i16 = 4;

/// How an order ended for its creator (`ContractCompletionReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderEnd {
    /// Not yet.
    None,
    /// Units were bought or sold.
    Succeeded,
    /// The creator canceled it.
    Canceled,
    /// Its time ran out.
    Expired,
}

impl OrderEnd {
    /// The `ContractCompletionReason` value.
    pub const fn value(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Succeeded => 1,
            Self::Canceled => 3,
            Self::Expired => 4,
        }
    }

    const fn column(self) -> i16 {
        match self {
            Self::None => 0,
            Self::Succeeded => 1,
            Self::Canceled => 3,
            Self::Expired => 4,
        }
    }

    const fn of_column(column: i16) -> Option<Self> {
        match column {
            0 => Some(Self::None),
            1 => Some(Self::Succeeded),
            3 => Some(Self::Canceled),
            4 => Some(Self::Expired),
            _ => None,
        }
    }
}

/// The item of an order, with the fields the search filters on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderItem {
    /// `ItemID`.
    pub item_id: String,
    /// The CRC-32 of the lowercase item id.
    pub item_crc: u32,
    /// `TradingCategory`.
    pub category: String,
    /// `TradingFamily`.
    pub family: String,
    /// `TradingGroup`.
    pub group: String,
    /// The tier.
    pub tier: u32,
    /// The gear score.
    pub gear_score: u32,
    /// The required level; `None` for none.
    pub required_level: Option<u32>,
    /// The perks (CRC-32 of each lowercase perk id).
    pub perks: Vec<u32>,
    /// The gem perk count.
    pub gem_perk_count: u32,
}

/// A new order.
#[derive(Debug, Clone, PartialEq)]
pub struct NewOrder {
    /// Its id.
    pub id: Uuid,
    /// Buy or sell.
    pub kind: OrderKind,
    /// The creator.
    pub creator: Uuid,
    /// The settlement.
    pub outpost: u64,
    /// The item.
    pub item: OrderItem,
    /// The price of one unit, hundredths.
    pub price: u64,
    /// Units.
    pub quantity: u32,
    /// The settlement's trading tax, a fraction below 1.
    pub tax_rate: f64,
    /// Unix ms of the posting.
    pub created_ms: u64,
    /// Unix ms when it expires.
    pub expires_ms: u64,
    /// The posting fee the creator paid, hundredths (for the audit).
    pub posting_fee: u64,
    /// Most open orders a character may have; the post is refused at this many.
    pub max_open: u32,
}

/// An order as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Order {
    /// Its id.
    pub id: Uuid,
    /// Buy or sell.
    pub kind: OrderKind,
    /// Where it is.
    pub status: OrderStatus,
    /// Grows with each change.
    pub version: u32,
    /// The creator.
    pub creator: Uuid,
    /// The settlement.
    pub outpost: u64,
    /// The item.
    pub item: OrderItem,
    /// The price of one unit, hundredths.
    pub price: u64,
    /// Units posted.
    pub posted: u32,
    /// Units still open (the units left at a cancel or expiry).
    pub open: u32,
    /// How it ended.
    pub end: OrderEnd,
    /// The settlement's trading tax at posting.
    pub tax_rate: f64,
    /// Unix ms of the posting.
    pub created_ms: u64,
    /// Unix ms when it expires.
    pub expires_ms: u64,
    /// Unix ms of its completion, cancel or expiry.
    pub completed_ms: Option<u64>,
    /// Unix ms of its last change.
    pub updated_ms: u64,
}

impl Order {
    /// Units bought or sold so far.
    pub const fn completed_units(&self) -> u32 {
        self.posted.saturating_sub(self.open)
    }
}

/// Why an order call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderRefusal {
    /// No such order.
    NoSuchOrder,
    /// The creator has the most open orders already.
    TooManyOrders,
    /// The order is no longer open.
    NotOpen,
    /// The order has fewer units open than asked.
    NotEnoughUnits,
    /// A creator cannot complete its own order.
    OwnOrder,
    /// Only the creator can cancel an order.
    NotCreator,
}

/// A completion that went through.
#[derive(Debug, Clone, PartialEq)]
pub struct Completed {
    /// The order after it.
    pub order: Order,
    /// Units done.
    pub units: u32,
    /// The price of those units, hundredths.
    pub total: u64,
    /// The tax the completer paid (a buyer, on top of the total) or had kept back (a seller), at
    /// its own rate.
    pub completer_tax: u64,
}

/// What a search asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderSearch {
    /// Buy or sell orders.
    pub kind: OrderKind,
    /// `TradingCategory`; `None` for any.
    pub category: Option<String>,
    /// `TradingFamily`; `None` for any.
    pub family: Option<String>,
    /// `TradingGroup`; `None` for any.
    pub group: Option<String>,
    /// `ItemID`; `None` for any.
    pub item_id: Option<String>,
    /// The tier; `None` for any.
    pub tier: Option<u32>,
    /// The settlements; empty for all.
    pub outposts: Vec<u64>,
    /// Perks an order's item must all have.
    pub perks: Vec<u32>,
    /// Matches to skip.
    pub start: u32,
    /// Most orders to return.
    pub limit: u32,
    /// Unix ms now: expired orders are left out.
    pub now_ms: u64,
}

/// One page of a search.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderPage {
    /// The orders, cheapest first.
    pub orders: Vec<Order>,
    /// Matches in all, at most [`MAX_COUNTED_MATCHES`].
    pub total: u32,
}

/// Which orders of a character a lookup wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterOrders {
    /// The character.
    pub character: Uuid,
    /// Open orders (available or completed with proceeds waiting).
    pub open: bool,
    /// Resolved orders.
    pub closed: bool,
    /// Orders it created.
    pub created: bool,
    /// Orders of others it completed.
    pub completed: bool,
    /// The kinds wanted.
    pub kinds: Vec<OrderKind>,
    /// Orders to skip.
    pub start: u32,
    /// Most orders to return.
    pub limit: u32,
}

/// What a delivery tells its character besides coins and items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryNotice {
    /// Nothing.
    None,
    /// The character's order was fully completed.
    FullyCompleted,
    /// The character's order expired.
    Expired,
}

impl DeliveryNotice {
    const fn column(self) -> i16 {
        match self {
            Self::None => 0,
            Self::FullyCompleted => 1,
            Self::Expired => 2,
        }
    }

    const fn of_column(column: i16) -> Option<Self> {
        match column {
            0 => Some(Self::None),
            1 => Some(Self::FullyCompleted),
            2 => Some(Self::Expired),
            _ => None,
        }
    }
}

/// Coins and items the economy owes a character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The character.
    pub character: Uuid,
    /// The order it comes from; `None` for what a sim gives back after its player left.
    pub order: Option<Uuid>,
    /// What it tells.
    pub notice: DeliveryNotice,
    /// Coins, hundredths.
    pub coins: u64,
    /// The item and how many; `None` for coins alone.
    pub item: Option<(String, u32)>,
    /// The units of the completion that made it.
    pub completed_count: u32,
    /// The request id of the completing call.
    pub request: Option<Uuid>,
}

/// A delivery a sim took, with its order as it is after the take.
#[derive(Debug, Clone, PartialEq)]
pub struct TakenDelivery {
    /// The delivery.
    pub delivery: Delivery,
    /// The order; `None` for a delivery without one.
    pub order: Option<Order>,
}

/// One player trade's transfer, for the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeSide {
    /// The giving character.
    pub character: Uuid,
    /// Coins it gave, hundredths.
    pub coins: u64,
    /// Items it gave: id and quantity.
    pub items: Vec<(String, u32)>,
}

/// The columns of an order row.
#[derive(Debug, Clone, sqlx::FromRow)]
struct OrderRow {
    contract_id: Uuid,
    kind: i16,
    status: i16,
    version: i32,
    creator_id: Uuid,
    outpost: i64,
    item_id: String,
    item_crc: i64,
    category: String,
    family: String,
    item_group: String,
    tier: i32,
    gear_score: i32,
    required_level: i32,
    perks: Json<Vec<u32>>,
    gem_perk_count: i32,
    price: i64,
    quantity_posted: i32,
    quantity_open: i32,
    reason: i16,
    tax_rate: f64,
    created_ms: i64,
    expires_ms: i64,
    completed_ms: Option<i64>,
    updated_ms: i64,
}

/// The columns of a delivery row joined with its order.
#[derive(Debug, Clone, sqlx::FromRow)]
struct DeliveryRow {
    character_id: Uuid,
    contract_id: Option<Uuid>,
    notice: i16,
    coins: i64,
    item_id: Option<String>,
    quantity: i32,
    completed_count: i32,
    request_id: Option<Uuid>,
}

fn out_of_range(field: &'static str) -> DbError {
    DbError::OutOfRange { field }
}

fn unsigned<T: TryFrom<i64>>(value: i64, field: &'static str) -> Result<T, DbError> {
    T::try_from(value).map_err(|_| out_of_range(field))
}

fn signed<T: TryInto<i64>>(value: T, field: &'static str) -> Result<i64, DbError> {
    value.try_into().map_err(|_| out_of_range(field))
}

fn int<T: TryInto<i32>>(value: T, field: &'static str) -> Result<i32, DbError> {
    value.try_into().map_err(|_| out_of_range(field))
}

/// The stored form of an outpost id (its bits, as `BIGINT`).
const fn outpost_column(outpost: u64) -> i64 {
    i64::from_be_bytes(outpost.to_be_bytes())
}

const fn outpost_of_column(column: i64) -> u64 {
    u64::from_be_bytes(column.to_be_bytes())
}

/// Unix ms as the seconds `to_timestamp` takes.
fn seconds(ms: u64) -> f64 {
    ms as f64 / MILLIS_PER_SECOND
}

impl OrderRow {
    fn order(self) -> Result<Order, DbError> {
        Ok(Order {
            id: self.contract_id,
            kind: OrderKind::of_column(self.kind).ok_or_else(|| out_of_range("kind"))?,
            status: OrderStatus::of_column(self.status).ok_or_else(|| out_of_range("status"))?,
            version: unsigned(i64::from(self.version), "version")?,
            creator: self.creator_id,
            outpost: outpost_of_column(self.outpost),
            item: OrderItem {
                item_id: self.item_id,
                item_crc: unsigned(self.item_crc, "item_crc")?,
                category: self.category,
                family: self.family,
                group: self.item_group,
                tier: unsigned(i64::from(self.tier), "tier")?,
                gear_score: unsigned(i64::from(self.gear_score), "gear_score")?,
                required_level: (self.required_level != NO_REQUIRED_LEVEL)
                    .then(|| unsigned(i64::from(self.required_level), "required_level"))
                    .transpose()?,
                perks: self.perks.0,
                gem_perk_count: unsigned(i64::from(self.gem_perk_count), "gem_perk_count")?,
            },
            price: unsigned(self.price, "price")?,
            posted: unsigned(i64::from(self.quantity_posted), "quantity_posted")?,
            open: unsigned(i64::from(self.quantity_open), "quantity_open")?,
            end: OrderEnd::of_column(self.reason).ok_or_else(|| out_of_range("reason"))?,
            tax_rate: self.tax_rate,
            created_ms: unsigned(self.created_ms, "created_at")?,
            expires_ms: unsigned(self.expires_ms, "expires_at")?,
            completed_ms: self
                .completed_ms
                .map(|ms| unsigned(ms, "completed_at"))
                .transpose()?,
            updated_ms: unsigned(self.updated_ms, "updated_at")?,
        })
    }
}

impl DeliveryRow {
    fn delivery(self) -> Result<Delivery, DbError> {
        let quantity: u32 = unsigned(i64::from(self.quantity), "quantity")?;
        Ok(Delivery {
            character: self.character_id,
            order: self.contract_id,
            notice: DeliveryNotice::of_column(self.notice).ok_or_else(|| out_of_range("notice"))?,
            coins: unsigned(self.coins, "coins")?,
            item: self.item_id.map(|item| (item, quantity)),
            completed_count: unsigned(i64::from(self.completed_count), "completed_count")?,
            request: self.request_id,
        })
    }
}

/// True when `error` is a serialization failure: the transfer may be tried again.
fn is_serialization_failure(error: &DbError) -> bool {
    match error {
        DbError::Database(error) => error
            .as_database_error()
            .and_then(|database| database.code())
            .is_some_and(|code| code == SERIALIZATION_FAILURE),
        _ => false,
    }
}

async fn serializable(world: &PgPool) -> Result<Transaction<'static, Postgres>, DbError> {
    let mut transaction = world.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

/// Runs `attempt` until it does not fail with a serialization failure, at most
/// [`TRANSFER_ATTEMPTS`] times.
macro_rules! with_retries {
    ($attempt:expr) => {{
        let mut tries = 1;
        loop {
            match $attempt.await {
                Err(error) if tries < TRANSFER_ATTEMPTS && is_serialization_failure(&error) => {
                    tries += 1;
                }
                other => break other,
            }
        }
    }};
}

async fn audit(
    transaction: &mut Transaction<'static, Postgres>,
    kind: &str,
    character: Uuid,
    other: Option<Uuid>,
    order: Option<Uuid>,
    coins: i64,
    item: Option<(&str, i32)>,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO economy_audit \
         (kind, character_id, other_character_id, contract_id, coins, item_id, quantity) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(kind)
    .bind(character)
    .bind(other)
    .bind(order)
    .bind(coins)
    .bind(item.map(|(item, _)| item))
    .bind(item.map_or(0, |(_, quantity)| quantity))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn deliver(
    transaction: &mut Transaction<'static, Postgres>,
    delivery: &Delivery,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO trading_post_delivery \
         (character_id, contract_id, notice, coins, item_id, quantity, completed_count, \
          request_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(delivery.character)
    .bind(delivery.order)
    .bind(delivery.notice.column())
    .bind(signed(delivery.coins, "coins")?)
    .bind(delivery.item.as_ref().map(|(item, _)| item.as_str()))
    .bind(int(
        delivery.item.as_ref().map_or(0, |(_, quantity)| *quantity),
        "quantity",
    )?)
    .bind(int(delivery.completed_count, "completed_count")?)
    .bind(delivery.request)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn locked_order(
    transaction: &mut Transaction<'static, Postgres>,
    order: Uuid,
) -> Result<Option<Order>, DbError> {
    let row: Option<OrderRow> = sqlx::query_as(concat!(
        "SELECT ",
        order_columns!(),
        " FROM trading_post_contract WHERE contract_id = $1 FOR UPDATE"
    ))
    .bind(order)
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(OrderRow::order).transpose()
}

/// Stores the change of `order`: its status, open units, end and times, one version up.
async fn update_order(
    transaction: &mut Transaction<'static, Postgres>,
    order: &Order,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE trading_post_contract SET status = $2, version = $3, quantity_open = $4, \
         reason = $5, completed_at = to_timestamp($6), updated_at = to_timestamp($7) \
         WHERE contract_id = $1",
    )
    .bind(order.id)
    .bind(order.status.column())
    .bind(int(order.version, "version")?)
    .bind(int(order.open, "quantity_open")?)
    .bind(order.end.column())
    .bind(order.completed_ms.map(seconds))
    .bind(seconds(order.updated_ms))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// The coins or items that `units` of `order` are worth to whoever gets them back or receives
/// them: a buy order's coins, a sell order's items.
fn order_units(order: &Order, units: u32) -> Result<(u64, Option<(String, u32)>), DbError> {
    Ok(match order.kind {
        OrderKind::Buy => (
            order
                .price
                .checked_mul(u64::from(units))
                .ok_or_else(|| out_of_range("price"))?,
            None,
        ),
        OrderKind::Sell => (0, Some((order.item.item_id.clone(), units))),
    })
}

/// Posts `order`. The sim took the items of a sell order, or the price of a buy order, and the
/// posting fee from the creator before.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (the post is not stored) and
/// [`DbError::OutOfRange`] for a value outside its column.
pub async fn post_order(
    world: &PgPool,
    order: &NewOrder,
) -> Result<Result<Order, OrderRefusal>, DbError> {
    with_retries!(try_post_order(world, order))
}

async fn try_post_order(
    world: &PgPool,
    order: &NewOrder,
) -> Result<Result<Order, OrderRefusal>, DbError> {
    let mut transaction = serializable(world).await?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM trading_post_contract WHERE creator_id = $1 AND status = $2",
    )
    .bind(order.creator)
    .bind(STATUS_AVAILABLE)
    .fetch_one(&mut *transaction)
    .await?;
    if open >= i64::from(order.max_open) {
        return Ok(Err(OrderRefusal::TooManyOrders));
    }
    let quantity = int(order.quantity, "quantity_posted")?;
    let row: OrderRow = sqlx::query_as(concat!(
        "INSERT INTO trading_post_contract (contract_id, kind, status, version, creator_id, \
         outpost, item_id, item_crc, category, family, item_group, tier, gear_score, \
         required_level, perks, gem_perk_count, price, quantity_posted, quantity_open, reason, \
         tax_rate, created_at, expires_at, updated_at) VALUES ($1, $2, $3, 1, $4, $5, $6, $7, \
         $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $17, 0, $18, to_timestamp($19), \
         to_timestamp($20), to_timestamp($19)) RETURNING ",
        order_columns!()
    ))
    .bind(order.id)
    .bind(order.kind.column())
    .bind(STATUS_AVAILABLE)
    .bind(order.creator)
    .bind(outpost_column(order.outpost))
    .bind(&order.item.item_id)
    .bind(i64::from(order.item.item_crc))
    .bind(&order.item.category)
    .bind(&order.item.family)
    .bind(&order.item.group)
    .bind(int(order.item.tier, "tier")?)
    .bind(int(order.item.gear_score, "gear_score")?)
    .bind(
        order
            .item
            .required_level
            .map_or(Ok(NO_REQUIRED_LEVEL), |level| int(level, "required_level"))?,
    )
    .bind(Json(&order.item.perks))
    .bind(int(order.item.gem_perk_count, "gem_perk_count")?)
    .bind(signed(order.price, "price")?)
    .bind(quantity)
    .bind(order.tax_rate)
    .bind(seconds(order.created_ms))
    .bind(seconds(order.expires_ms))
    .fetch_one(&mut *transaction)
    .await?;
    let posted = row.order()?;
    let (escrow, items) = order_units(&posted, order.quantity)?;
    let paid = escrow
        .checked_add(order.posting_fee)
        .ok_or_else(|| out_of_range("coins"))?;
    audit(
        &mut transaction,
        AUDIT_POST,
        order.creator,
        None,
        Some(order.id),
        -signed(paid, "coins")?,
        items
            .as_ref()
            .map(|(item, quantity)| (item.as_str(), -i32::try_from(*quantity).unwrap_or(i32::MAX))),
    )
    .await?;
    transaction.commit().await?;
    Ok(Ok(posted))
}

/// Completes `units` of order `order` for `completer` (request id `request`, trading tax
/// `completer_rate`) at `now_ms`: a buyer of a sell order paid the total and its tax at
/// `completer_rate` before; a seller to a buy order gave the items before. The tax the completer
/// pays or has kept back is at `completer_rate`; the creator of a sell order has the tax at the
/// order's rate kept back (retail `buy-sell-TP` and `Hivesellandbuyorder`, one settlement: the
/// buyer and the seller to a buy order paid 2.35 % for one character and 5 % for the other, 4 of
/// 4; each sell order's creator got 95 %, 2 of 2). What each side gets waits as deliveries
/// ([`take_deliveries`]).
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (nothing changes) and
/// [`DbError::OutOfRange`] for a total outside the coin range.
pub async fn complete_order(
    world: &PgPool,
    order: Uuid,
    completer: Uuid,
    units: u32,
    completer_rate: f64,
    request: Uuid,
    now_ms: u64,
) -> Result<Result<Completed, OrderRefusal>, DbError> {
    with_retries!(try_complete_order(
        world,
        order,
        completer,
        units,
        completer_rate,
        request,
        now_ms
    ))
}

async fn try_complete_order(
    world: &PgPool,
    order: Uuid,
    completer: Uuid,
    units: u32,
    completer_rate: f64,
    request: Uuid,
    now_ms: u64,
) -> Result<Result<Completed, OrderRefusal>, DbError> {
    let mut transaction = serializable(world).await?;
    let Some(mut stored) = locked_order(&mut transaction, order).await? else {
        return Ok(Err(OrderRefusal::NoSuchOrder));
    };
    if stored.status != OrderStatus::Available || stored.expires_ms <= now_ms {
        return Ok(Err(OrderRefusal::NotOpen));
    }
    if stored.creator == completer {
        return Ok(Err(OrderRefusal::OwnOrder));
    }
    if units == 0 || stored.open < units {
        return Ok(Err(OrderRefusal::NotEnoughUnits));
    }
    let total = stored
        .price
        .checked_mul(u64::from(units))
        .ok_or_else(|| out_of_range("price"))?;
    let tax = tax_of(total, completer_rate);
    let creator_tax = tax_of(total, stored.tax_rate);
    stored.open -= units;
    stored.version += 1;
    stored.updated_ms = now_ms;
    stored.end = OrderEnd::Succeeded;
    let fully = stored.open == 0;
    if fully {
        stored.status = OrderStatus::Completed;
        stored.completed_ms = Some(now_ms);
    }
    update_order(&mut transaction, &stored).await?;
    sqlx::query(
        "INSERT INTO trading_post_completion (contract_id, character_id, quantity, completed_at) \
         VALUES ($1, $2, $3, to_timestamp($4))",
    )
    .bind(order)
    .bind(completer)
    .bind(int(units, "quantity")?)
    .bind(seconds(now_ms))
    .execute(&mut *transaction)
    .await?;
    let notice = if fully {
        DeliveryNotice::FullyCompleted
    } else {
        DeliveryNotice::None
    };
    let item = (stored.item.item_id.clone(), units);
    let signed_units = int(units, "quantity")?;
    let (creator_gets, completer_gets, completer_coins, completer_kind) = match stored.kind {
        OrderKind::Sell => (
            Delivery {
                character: stored.creator,
                order: Some(order),
                notice,
                coins: total - creator_tax,
                item: None,
                completed_count: units,
                request: Some(request),
            },
            Delivery {
                character: completer,
                order: Some(order),
                notice: DeliveryNotice::None,
                coins: 0,
                item: Some(item.clone()),
                completed_count: units,
                request: Some(request),
            },
            -signed(
                total
                    .checked_add(tax)
                    .ok_or_else(|| out_of_range("coins"))?,
                "coins",
            )?,
            AUDIT_BUY,
        ),
        OrderKind::Buy => (
            Delivery {
                character: stored.creator,
                order: Some(order),
                notice,
                coins: 0,
                item: Some(item.clone()),
                completed_count: units,
                request: Some(request),
            },
            Delivery {
                character: completer,
                order: Some(order),
                notice: DeliveryNotice::None,
                coins: total - tax,
                item: None,
                completed_count: units,
                request: Some(request),
            },
            signed(total - tax, "coins")?,
            AUDIT_SELL,
        ),
    };
    deliver(&mut transaction, &creator_gets).await?;
    deliver(&mut transaction, &completer_gets).await?;
    let completer_items = match stored.kind {
        OrderKind::Sell => signed_units,
        OrderKind::Buy => -signed_units,
    };
    audit(
        &mut transaction,
        completer_kind,
        completer,
        Some(stored.creator),
        Some(order),
        completer_coins,
        Some((item.0.as_str(), completer_items)),
    )
    .await?;
    transaction.commit().await?;
    Ok(Ok(Completed {
        order: stored,
        units,
        total,
        completer_tax: tax,
    }))
}

/// Cancels order `order` of `creator` at `now_ms`: the units left go back as a delivery (a buy
/// order's coins, a sell order's items).
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (nothing changes).
pub async fn cancel_order(
    world: &PgPool,
    order: Uuid,
    creator: Uuid,
    now_ms: u64,
) -> Result<Result<Order, OrderRefusal>, DbError> {
    with_retries!(try_cancel_order(world, order, creator, now_ms))
}

async fn try_cancel_order(
    world: &PgPool,
    order: Uuid,
    creator: Uuid,
    now_ms: u64,
) -> Result<Result<Order, OrderRefusal>, DbError> {
    let mut transaction = serializable(world).await?;
    let Some(mut stored) = locked_order(&mut transaction, order).await? else {
        return Ok(Err(OrderRefusal::NoSuchOrder));
    };
    if stored.creator != creator {
        return Ok(Err(OrderRefusal::NotCreator));
    }
    if stored.status != OrderStatus::Available {
        return Ok(Err(OrderRefusal::NotOpen));
    }
    close_order(
        &mut transaction,
        &mut stored,
        OrderEnd::Canceled,
        DeliveryNotice::None,
        now_ms,
    )
    .await?;
    transaction.commit().await?;
    Ok(Ok(stored))
}

/// Resolves `order` with `end` at `now_ms` and owes its creator the units left, with `notice`.
async fn close_order(
    transaction: &mut Transaction<'static, Postgres>,
    order: &mut Order,
    end: OrderEnd,
    notice: DeliveryNotice,
    now_ms: u64,
) -> Result<(), DbError> {
    order.status = OrderStatus::Resolved;
    order.end = end;
    order.version += 1;
    order.completed_ms = Some(now_ms);
    order.updated_ms = now_ms;
    update_order(transaction, order).await?;
    let (coins, item) = order_units(order, order.open)?;
    deliver(
        transaction,
        &Delivery {
            character: order.creator,
            order: Some(order.id),
            notice,
            coins,
            item: item.filter(|(_, quantity)| *quantity > 0),
            completed_count: 0,
            request: None,
        },
    )
    .await?;
    let kind = match end {
        OrderEnd::Expired => AUDIT_EXPIRE,
        OrderEnd::None | OrderEnd::Succeeded | OrderEnd::Canceled => AUDIT_CANCEL,
    };
    audit(
        transaction,
        kind,
        order.creator,
        None,
        Some(order.id),
        0,
        None,
    )
    .await
}

/// Expires at most `limit` open orders whose time ran out by `now_ms`: each is resolved, and its
/// creator is owed the units left with an expiry notice. Returns how many expired.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (nothing changes).
pub async fn expire_orders(world: &PgPool, now_ms: u64, limit: u32) -> Result<u32, DbError> {
    with_retries!(try_expire_orders(world, now_ms, limit))
}

async fn try_expire_orders(world: &PgPool, now_ms: u64, limit: u32) -> Result<u32, DbError> {
    let mut transaction = serializable(world).await?;
    let rows: Vec<OrderRow> = sqlx::query_as(concat!(
        "SELECT ",
        order_columns!(),
        " FROM trading_post_contract WHERE status = $1 AND expires_at <= to_timestamp($2) \
         ORDER BY expires_at LIMIT $3 FOR UPDATE SKIP LOCKED"
    ))
    .bind(STATUS_AVAILABLE)
    .bind(seconds(now_ms))
    .bind(i64::from(limit))
    .fetch_all(&mut *transaction)
    .await?;
    let mut expired = 0;
    for row in rows {
        let mut order = row.order()?;
        close_order(
            &mut transaction,
            &mut order,
            OrderEnd::Expired,
            DeliveryNotice::Expired,
            now_ms,
        )
        .await?;
        expired += 1;
    }
    transaction.commit().await?;
    Ok(expired)
}

/// One page of the open orders that match `search`, cheapest first (then the earliest to
/// expire).
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn search_orders(world: &PgPool, search: &OrderSearch) -> Result<OrderPage, DbError> {
    let outposts: Vec<i64> = search
        .outposts
        .iter()
        .map(|outpost| outpost_column(*outpost))
        .collect();
    let tier = search.tier.map(|tier| int(tier, "tier")).transpose()?;
    let rows: Vec<OrderRow> = bind_search!(
        sqlx::query_as(concat!(
            "SELECT ",
            order_columns!(),
            " FROM trading_post_contract WHERE ",
            search_filter!(),
            " ORDER BY price, expires_at, contract_id LIMIT $11 OFFSET $12"
        )),
        search,
        outposts,
        tier
    )
    .bind(i64::from(search.limit))
    .bind(i64::from(search.start))
    .fetch_all(world)
    .await?;
    let total: i64 = bind_search!(
        sqlx::query_scalar(concat!(
            "SELECT count(*) FROM (SELECT 1 FROM trading_post_contract WHERE ",
            search_filter!(),
            " LIMIT $11) AS matched"
        )),
        search,
        outposts,
        tier
    )
    .bind(MAX_COUNTED_MATCHES)
    .fetch_one(world)
    .await?;
    Ok(OrderPage {
        orders: rows
            .into_iter()
            .map(OrderRow::order)
            .collect::<Result<_, _>>()?,
        total: unsigned(total, "total")?,
    })
}

/// The orders of `ids` that exist, in no set order.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn orders_by_id(world: &PgPool, ids: &[Uuid]) -> Result<Vec<Order>, DbError> {
    let rows: Vec<OrderRow> = sqlx::query_as(concat!(
        "SELECT ",
        order_columns!(),
        " FROM trading_post_contract WHERE contract_id = ANY($1)"
    ))
    .bind(ids)
    .fetch_all(world)
    .await?;
    rows.into_iter().map(OrderRow::order).collect()
}

/// The orders `wanted` names, newest change first: those its character created and those of
/// others it completed. Each comes with whether the character created it.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn character_orders(
    world: &PgPool,
    wanted: &CharacterOrders,
) -> Result<Vec<(Order, bool)>, DbError> {
    let mut statuses = Vec::new();
    if wanted.open {
        statuses.extend([STATUS_AVAILABLE, STATUS_COMPLETED]);
    }
    if wanted.closed {
        statuses.push(STATUS_RESOLVED);
    }
    let kinds: Vec<i16> = wanted.kinds.iter().map(|kind| kind.column()).collect();
    let rows: Vec<OrderRow> = sqlx::query_as(concat!(
        "SELECT ",
        order_columns!(),
        " FROM trading_post_contract WHERE status = ANY($2) AND kind = ANY($3) AND \
         (($4 AND creator_id = $1) OR ($5 AND contract_id IN \
          (SELECT contract_id FROM trading_post_completion WHERE character_id = $1))) \
         ORDER BY updated_at DESC, contract_id LIMIT $6 OFFSET $7"
    ))
    .bind(wanted.character)
    .bind(statuses)
    .bind(kinds)
    .bind(wanted.created)
    .bind(wanted.completed)
    .bind(i64::from(wanted.limit))
    .bind(i64::from(wanted.start))
    .fetch_all(world)
    .await?;
    rows.into_iter()
        .map(|row| {
            let order = row.order()?;
            let created = order.creator == wanted.character;
            Ok((order, created))
        })
        .collect()
}

/// Takes every delivery owed to `characters`: the rows are removed, and a fully completed order
/// whose proceeds its creator takes is resolved. Each comes with its order as it is now.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (nothing is taken).
pub async fn take_deliveries(
    world: &PgPool,
    characters: &[Uuid],
) -> Result<Vec<TakenDelivery>, DbError> {
    with_retries!(try_take_deliveries(world, characters))
}

async fn try_take_deliveries(
    world: &PgPool,
    characters: &[Uuid],
) -> Result<Vec<TakenDelivery>, DbError> {
    let mut transaction = serializable(world).await?;
    let rows: Vec<DeliveryRow> = sqlx::query_as(
        "DELETE FROM trading_post_delivery WHERE character_id = ANY($1) RETURNING \
         character_id, contract_id, notice, coins, item_id, quantity, completed_count, \
         request_id",
    )
    .bind(characters)
    .fetch_all(&mut *transaction)
    .await?;
    if rows.is_empty() {
        transaction.commit().await?;
        return Ok(Vec::new());
    }
    let deliveries: Vec<Delivery> = rows
        .into_iter()
        .map(DeliveryRow::delivery)
        .collect::<Result<_, _>>()?;
    let finished: Vec<Uuid> = deliveries
        .iter()
        .filter(|delivery| delivery.notice == DeliveryNotice::FullyCompleted)
        .filter_map(|delivery| delivery.order)
        .collect();
    sqlx::query(
        "UPDATE trading_post_contract SET status = $2, version = version + 1, \
         updated_at = now() WHERE contract_id = ANY($1) AND status = $3",
    )
    .bind(&finished)
    .bind(STATUS_RESOLVED)
    .bind(STATUS_COMPLETED)
    .execute(&mut *transaction)
    .await?;
    let ids: Vec<Uuid> = deliveries
        .iter()
        .filter_map(|delivery| delivery.order)
        .collect();
    let orders: Vec<OrderRow> = sqlx::query_as(concat!(
        "SELECT ",
        order_columns!(),
        " FROM trading_post_contract WHERE contract_id = ANY($1)"
    ))
    .bind(&ids)
    .fetch_all(&mut *transaction)
    .await?;
    let orders: Vec<Order> = orders
        .into_iter()
        .map(OrderRow::order)
        .collect::<Result<_, _>>()?;
    let mut taken = Vec::with_capacity(deliveries.len());
    for delivery in deliveries {
        let order = delivery
            .order
            .and_then(|id| orders.iter().find(|order| order.id == id).cloned());
        audit(
            &mut transaction,
            AUDIT_DELIVERED,
            delivery.character,
            None,
            delivery.order,
            signed(delivery.coins, "coins")?,
            delivery.item.as_ref().map(|(item, quantity)| {
                (item.as_str(), i32::try_from(*quantity).unwrap_or(i32::MAX))
            }),
        )
        .await?;
        taken.push(TakenDelivery { delivery, order });
    }
    transaction.commit().await?;
    Ok(taken)
}

/// Owes `deliveries` again: the parts of taken deliveries the character could not receive (a
/// full bag, a full wallet).
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (nothing is stored).
pub async fn return_deliveries(world: &PgPool, deliveries: &[Delivery]) -> Result<(), DbError> {
    let mut transaction = serializable(world).await?;
    for delivery in deliveries {
        deliver(&mut transaction, delivery).await?;
        audit(
            &mut transaction,
            AUDIT_DELIVERED,
            delivery.character,
            None,
            delivery.order,
            -signed(delivery.coins, "coins")?,
            delivery.item.as_ref().map(|(item, quantity)| {
                (item.as_str(), -i32::try_from(*quantity).unwrap_or(i32::MAX))
            }),
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

/// Writes the audit rows of one player trade: what each side gave the other.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (no row is stored).
pub async fn record_trade(
    world: &PgPool,
    first: &TradeSide,
    second: &TradeSide,
) -> Result<(), DbError> {
    let mut transaction = world.begin().await?;
    for (giver, taker) in [(first, second), (second, first)] {
        audit(
            &mut transaction,
            AUDIT_TRADE,
            giver.character,
            Some(taker.character),
            None,
            -signed(giver.coins, "coins")?,
            None,
        )
        .await?;
        for (item, quantity) in &giver.items {
            audit(
                &mut transaction,
                AUDIT_TRADE,
                giver.character,
                Some(taker.character),
                None,
                0,
                Some((item.as_str(), -int(*quantity, "quantity")?)),
            )
            .await?;
        }
    }
    transaction.commit().await?;
    Ok(())
}

/// Claims a purchase of shop offer `conversion_id` with a cooldown of `cooldown_ms` by
/// `character` at `now_ms`: stored when the character never bought it or its last purchase is at
/// least the cooldown old. Returns `None` while the cooldown runs, else the time of the purchase
/// before (`None` for the first), which [`release_shop_purchase`] puts back.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (nothing is stored).
pub async fn claim_shop_purchase(
    world: &PgPool,
    character: Uuid,
    conversion_id: &str,
    now_ms: u64,
    cooldown_ms: u64,
) -> Result<Option<Option<u64>>, DbError> {
    let claimed: Option<(Option<i64>,)> = sqlx::query_as(
        "WITH previous AS (SELECT bought_at FROM shop_purchase \
         WHERE character_id = $1 AND conversion_id = $2) \
         INSERT INTO shop_purchase (character_id, conversion_id, bought_at) \
         VALUES ($1, $2, to_timestamp($3)) \
         ON CONFLICT (character_id, conversion_id) DO UPDATE SET bought_at = EXCLUDED.bought_at \
         WHERE shop_purchase.bought_at <= to_timestamp($3 - $4) \
         RETURNING (SELECT (EXTRACT(EPOCH FROM bought_at) * 1000)::BIGINT FROM previous)",
    )
    .bind(character)
    .bind(conversion_id)
    .bind(seconds(now_ms))
    .bind(seconds(cooldown_ms))
    .fetch_optional(world)
    .await?;
    claimed
        .map(|(previous,)| previous.map(|ms| unsigned(ms, "bought_at")).transpose())
        .transpose()
}

/// Gives back a purchase that [`claim_shop_purchase`] claimed at `claimed_ms` and that did not
/// go through: the purchase before (`previous_ms`) is the last again, or none.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (the claim stays).
pub async fn release_shop_purchase(
    world: &PgPool,
    character: Uuid,
    conversion_id: &str,
    claimed_ms: u64,
    previous_ms: Option<u64>,
) -> Result<(), DbError> {
    let query = match previous_ms {
        Some(previous_ms) => sqlx::query(
            "UPDATE shop_purchase SET bought_at = to_timestamp($4) \
             WHERE character_id = $1 AND conversion_id = $2 AND bought_at = to_timestamp($3)",
        )
        .bind(character)
        .bind(conversion_id)
        .bind(seconds(claimed_ms))
        .bind(seconds(previous_ms)),
        None => sqlx::query(
            "DELETE FROM shop_purchase \
             WHERE character_id = $1 AND conversion_id = $2 AND bought_at = to_timestamp($3)",
        )
        .bind(character)
        .bind(conversion_id)
        .bind(seconds(claimed_ms)),
    };
    query.execute(world).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retail taxes of the captured trades (economy note, section 3).
    #[test]
    fn taxes_round_down_as_retail() {
        const BEESWAX_PAIR: u64 = 20_000;
        const REDUCED_RATE: f64 = 0.0235;
        const SETTLEMENT_RATE: f64 = 0.05;
        assert_eq!(tax_of(BEESWAX_PAIR, REDUCED_RATE), 470);
        assert_eq!(tax_of(10_000, SETTLEMENT_RATE), 500);
        assert_eq!(tax_of(992, SETTLEMENT_RATE), 49);
        assert_eq!(tax_of(10_000, 0.0), 0);
    }

    #[test]
    fn outpost_ids_keep_every_bit() {
        for outpost in [0, 1, 0xb325_909b_3cc6_434f, u64::MAX] {
            assert_eq!(outpost_of_column(outpost_column(outpost)), outpost);
        }
    }

    #[test]
    fn stored_codes_read_back() {
        for kind in [OrderKind::Buy, OrderKind::Sell] {
            assert_eq!(OrderKind::of_column(kind.column()), Some(kind));
        }
        for status in [
            OrderStatus::Available,
            OrderStatus::Completed,
            OrderStatus::Resolved,
        ] {
            assert_eq!(OrderStatus::of_column(status.column()), Some(status));
        }
        for end in [
            OrderEnd::None,
            OrderEnd::Succeeded,
            OrderEnd::Canceled,
            OrderEnd::Expired,
        ] {
            assert_eq!(OrderEnd::of_column(end.column()), Some(end));
        }
        for notice in [
            DeliveryNotice::None,
            DeliveryNotice::FullyCompleted,
            DeliveryNotice::Expired,
        ] {
            assert_eq!(DeliveryNotice::of_column(notice.column()), Some(notice));
        }
    }
}

#[cfg(all(test, feature = "infra"))]
mod infra_tests {
    use super::*;
    use crate::migrate::WORLD_MIGRATOR;
    use crate::test_db::with_test_schema;
    use crate::{NewCharacter, insert_character};

    /// 2027-01-01.
    const NOW_MS: u64 = 1_798_761_600_000;
    const DAY_MS: u64 = 86_400_000;
    const PRICE: u64 = 10_000;
    const RATE: f64 = 0.05;
    const MAX_OPEN: u32 = 40;

    async fn character(pool: &PgPool, name: &str) -> Uuid {
        insert_character(
            pool,
            &NewCharacter {
                id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                name: name.to_owned(),
                appearance: serde_json::json!({}),
            },
        )
        .await
        .expect("insert")
        .id
    }

    fn order(kind: OrderKind, creator: Uuid, quantity: u32) -> NewOrder {
        NewOrder {
            id: Uuid::new_v4(),
            kind,
            creator,
            outpost: 0xb325_909b_3cc6_434f,
            item: OrderItem {
                item_id: "BeeswaxT1".to_owned(),
                item_crc: 0xd0a8_26e7,
                category: "Resources".to_owned(),
                family: "RawResources".to_owned(),
                group: "Beeswax".to_owned(),
                tier: 1,
                gear_score: 0,
                required_level: None,
                perks: Vec::new(),
                gem_perk_count: 0,
            },
            price: PRICE,
            quantity,
            tax_rate: RATE,
            created_ms: NOW_MS,
            expires_ms: NOW_MS + 3 * DAY_MS,
            posting_fee: 0,
            max_open: MAX_OPEN,
        }
    }

    #[tokio::test]
    async fn a_sell_order_is_found_bought_and_paid_out() {
        let (page, completed, seller_gets, buyer_gets, again) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let seller = character(pool, "Seller").await;
            let buyer = character(pool, "Buyer").await;
            let posted = post_order(pool, &order(OrderKind::Sell, seller, 2))
                .await
                .expect("post")
                .expect("posted");
            let page = search_orders(
                pool,
                &OrderSearch {
                    kind: OrderKind::Sell,
                    category: Some("Resources".to_owned()),
                    family: None,
                    group: None,
                    item_id: Some("BeeswaxT1".to_owned()),
                    tier: None,
                    outposts: vec![posted.outpost],
                    perks: Vec::new(),
                    start: 0,
                    limit: 100,
                    now_ms: NOW_MS,
                },
            )
            .await
            .expect("search");
            let completed = complete_order(pool, posted.id, buyer, 2, RATE, Uuid::new_v4(), NOW_MS)
                .await
                .expect("complete")
                .expect("completed");
            let seller_gets = take_deliveries(pool, &[seller]).await.expect("take");
            let buyer_gets = take_deliveries(pool, &[buyer]).await.expect("take");
            let again = complete_order(pool, posted.id, buyer, 1, RATE, Uuid::new_v4(), NOW_MS)
                .await
                .expect("complete");
            (page, completed, seller_gets, buyer_gets, again)
        })
        .await;
        assert_eq!(page.total, 1);
        assert_eq!(page.orders.len(), 1);
        assert_eq!(completed.total, 2 * PRICE);
        assert_eq!(completed.completer_tax, tax_of(2 * PRICE, RATE));
        assert_eq!(completed.order.status, OrderStatus::Completed);
        assert_eq!(seller_gets.len(), 1);
        assert_eq!(
            seller_gets[0].delivery.coins,
            2 * PRICE - tax_of(2 * PRICE, RATE)
        );
        assert_eq!(
            seller_gets[0].delivery.notice,
            DeliveryNotice::FullyCompleted
        );
        assert_eq!(
            seller_gets[0].order.as_ref().map(|order| order.status),
            Some(OrderStatus::Resolved)
        );
        assert_eq!(
            buyer_gets[0].delivery.item,
            Some(("BeeswaxT1".to_owned(), 2))
        );
        assert_eq!(again, Err(OrderRefusal::NotOpen));
    }

    fn sell_search(now_ms: u64) -> OrderSearch {
        OrderSearch {
            kind: OrderKind::Sell,
            category: None,
            family: None,
            group: None,
            item_id: Some("BeeswaxT1".to_owned()),
            tier: None,
            outposts: Vec::new(),
            perks: Vec::new(),
            start: 0,
            limit: 100,
            now_ms,
        }
    }

    fn lists_of(character: Uuid) -> CharacterOrders {
        CharacterOrders {
            character,
            open: true,
            closed: true,
            created: true,
            completed: true,
            kinds: vec![OrderKind::Buy, OrderKind::Sell],
            start: 0,
            limit: 100,
        }
    }

    /// The owner's case: a stack of 10 listed, 1 bought, then the other 9. Each player sees
    /// what is left after each buy; the buyer gets all 10 and the seller the price of all 10
    /// less the tax at the order's rate, while the buyer paid the tax at its own rate.
    #[tokio::test]
    async fn a_stack_of_ten_is_bought_one_then_nine() {
        const STACK: u32 = 10;
        const FIRST_BUY: u32 = 1;
        const BUYER_RATE: f64 = 0.0235;
        let (
            after_one,
            seller_list,
            buyer_list,
            after_all,
            first,
            rest,
            seller_gets,
            buyer_gets,
            closed,
        ) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let seller = character(pool, "Seller").await;
            let buyer = character(pool, "Buyer").await;
            let posted = post_order(pool, &order(OrderKind::Sell, seller, STACK))
                .await
                .expect("post")
                .expect("posted");
            let first = complete_order(
                pool,
                posted.id,
                buyer,
                FIRST_BUY,
                BUYER_RATE,
                Uuid::new_v4(),
                NOW_MS,
            )
            .await
            .expect("complete")
            .expect("completed");
            let after_one = search_orders(pool, &sell_search(NOW_MS))
                .await
                .expect("search");
            let seller_list = character_orders(pool, &lists_of(seller))
                .await
                .expect("list");
            let buyer_list = character_orders(pool, &lists_of(buyer))
                .await
                .expect("list");
            let rest = complete_order(
                pool,
                posted.id,
                buyer,
                STACK - FIRST_BUY,
                BUYER_RATE,
                Uuid::new_v4(),
                NOW_MS,
            )
            .await
            .expect("complete")
            .expect("completed");
            let after_all = search_orders(pool, &sell_search(NOW_MS))
                .await
                .expect("search");
            let seller_gets = take_deliveries(pool, &[seller]).await.expect("take");
            let buyer_gets = take_deliveries(pool, &[buyer]).await.expect("take");
            let closed = character_orders(pool, &lists_of(seller))
                .await
                .expect("list");
            (
                after_one,
                seller_list,
                buyer_list,
                after_all,
                first,
                rest,
                seller_gets,
                buyer_gets,
                closed,
            )
        })
        .await;
        let left = STACK - FIRST_BUY;
        assert_eq!(after_one.total, 1);
        assert_eq!(after_one.orders[0].open, left);
        assert_eq!(seller_list[0].0.open, left);
        assert!(seller_list[0].1, "the seller created it");
        assert_eq!(buyer_list[0].0.open, left);
        assert!(!buyer_list[0].1, "the buyer completed part of it");
        assert_eq!(first.order.status, OrderStatus::Available);
        assert_eq!(first.completer_tax, tax_of(PRICE, BUYER_RATE));
        assert_eq!(rest.order.status, OrderStatus::Completed);
        assert_eq!(rest.order.open, 0);
        assert_eq!(after_all.total, 0, "the listing is gone for everyone");
        assert!(after_all.orders.is_empty());
        let rest_total = u64::from(left) * PRICE;
        let coins: u64 = seller_gets.iter().map(|taken| taken.delivery.coins).sum();
        assert_eq!(
            coins,
            PRICE - tax_of(PRICE, RATE) + rest_total - tax_of(rest_total, RATE)
        );
        assert!(
            seller_gets
                .iter()
                .all(|taken| taken.delivery.item.is_none())
        );
        let items: u32 = buyer_gets
            .iter()
            .filter_map(|taken| taken.delivery.item.as_ref())
            .map(|(item, quantity)| {
                assert_eq!(item, "BeeswaxT1");
                *quantity
            })
            .sum();
        assert_eq!(items, STACK);
        assert!(buyer_gets.iter().all(|taken| taken.delivery.coins == 0));
        assert_eq!(closed[0].0.status, OrderStatus::Resolved);
        assert_eq!(closed[0].0.end, OrderEnd::Succeeded);
    }

    /// A buy order of 10 sold into twice: the creator gets every unit; each seller the price
    /// less the tax at its own rate. A cancel of a partly bought sell order gives back the rest.
    #[tokio::test]
    async fn a_buy_order_is_filled_in_parts_and_a_cancel_gives_back_the_rest() {
        const STACK: u32 = 10;
        const FIRST_SALE: u32 = 4;
        let (creator_gets, seller_gets, refund) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let creator = character(pool, "Wanting").await;
            let seller = character(pool, "Having").await;
            let buy = post_order(pool, &order(OrderKind::Buy, creator, STACK))
                .await
                .expect("post")
                .expect("posted");
            for units in [FIRST_SALE, STACK - FIRST_SALE] {
                complete_order(pool, buy.id, seller, units, RATE, Uuid::new_v4(), NOW_MS)
                    .await
                    .expect("complete")
                    .expect("completed");
            }
            let creator_gets = take_deliveries(pool, &[creator]).await.expect("take");
            let seller_gets = take_deliveries(pool, &[seller]).await.expect("take");
            let sell = post_order(pool, &order(OrderKind::Sell, creator, STACK))
                .await
                .expect("post")
                .expect("posted");
            complete_order(
                pool,
                sell.id,
                seller,
                FIRST_SALE,
                RATE,
                Uuid::new_v4(),
                NOW_MS,
            )
            .await
            .expect("complete")
            .expect("completed");
            take_deliveries(pool, &[creator]).await.expect("take");
            cancel_order(pool, sell.id, creator, NOW_MS)
                .await
                .expect("cancel")
                .expect("canceled");
            let refund = take_deliveries(pool, &[creator]).await.expect("take");
            (creator_gets, seller_gets, refund)
        })
        .await;
        let units: u32 = creator_gets
            .iter()
            .filter_map(|taken| taken.delivery.item.as_ref().map(|(_, quantity)| *quantity))
            .sum();
        assert_eq!(units, STACK);
        let first = u64::from(FIRST_SALE) * PRICE;
        let second = u64::from(STACK - FIRST_SALE) * PRICE;
        let coins: u64 = seller_gets.iter().map(|taken| taken.delivery.coins).sum();
        assert_eq!(
            coins,
            first - tax_of(first, RATE) + second - tax_of(second, RATE)
        );
        assert_eq!(refund.len(), 1);
        assert_eq!(
            refund[0].delivery.item,
            Some(("BeeswaxT1".to_owned(), STACK - FIRST_SALE))
        );
    }

    #[tokio::test]
    async fn cancels_and_expiries_give_the_rest_back() {
        let (refused, canceled, refund, expired, returned) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let creator = character(pool, "Creator").await;
            let other = character(pool, "Other").await;
            let buy = post_order(pool, &order(OrderKind::Buy, creator, 3))
                .await
                .expect("post")
                .expect("posted");
            let refused = cancel_order(pool, buy.id, other, NOW_MS)
                .await
                .expect("cancel");
            let canceled = cancel_order(pool, buy.id, creator, NOW_MS)
                .await
                .expect("cancel")
                .expect("canceled");
            let refund = take_deliveries(pool, &[creator]).await.expect("take");
            post_order(pool, &order(OrderKind::Sell, creator, 4))
                .await
                .expect("post")
                .expect("posted");
            let expired = expire_orders(pool, NOW_MS + 4 * DAY_MS, MAX_OPEN)
                .await
                .expect("expire");
            let returned = take_deliveries(pool, &[creator]).await.expect("take");
            (refused, canceled, refund, expired, returned)
        })
        .await;
        assert_eq!(refused, Err(OrderRefusal::NotCreator));
        assert_eq!(canceled.end, OrderEnd::Canceled);
        assert_eq!(refund[0].delivery.coins, 3 * PRICE);
        assert_eq!(expired, 1);
        assert_eq!(returned[0].delivery.notice, DeliveryNotice::Expired);
        assert_eq!(returned[0].delivery.item, Some(("BeeswaxT1".to_owned(), 4)));
    }

    #[tokio::test]
    async fn a_creator_cannot_post_beyond_its_limit() {
        let refused = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let creator = character(pool, "Busy").await;
            let mut first = order(OrderKind::Sell, creator, 1);
            first.max_open = 1;
            post_order(pool, &first)
                .await
                .expect("post")
                .expect("posted");
            let mut second = order(OrderKind::Sell, creator, 1);
            second.max_open = 1;
            post_order(pool, &second).await.expect("post")
        })
        .await;
        assert_eq!(refused, Err(OrderRefusal::TooManyOrders));
    }

    #[tokio::test]
    async fn a_shop_cooldown_holds_until_it_passed_and_a_release_gives_it_back() {
        const OFFER: &str = "PotionCleanseFTW";
        let results = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let buyer = character(pool, "Shopper").await;
            let first = claim_shop_purchase(pool, buyer, OFFER, NOW_MS, DAY_MS)
                .await
                .expect("claim");
            let early = claim_shop_purchase(pool, buyer, OFFER, NOW_MS + DAY_MS - 1, DAY_MS)
                .await
                .expect("claim");
            let later = claim_shop_purchase(pool, buyer, OFFER, NOW_MS + DAY_MS, DAY_MS)
                .await
                .expect("claim");
            release_shop_purchase(pool, buyer, OFFER, NOW_MS + DAY_MS, Some(NOW_MS))
                .await
                .expect("release");
            let after_release = claim_shop_purchase(pool, buyer, OFFER, NOW_MS + 1, DAY_MS)
                .await
                .expect("claim");
            release_shop_purchase(pool, buyer, OFFER, NOW_MS, None)
                .await
                .expect("release");
            let after_delete = claim_shop_purchase(pool, buyer, OFFER, NOW_MS + 1, DAY_MS)
                .await
                .expect("claim");
            (first, early, later, after_release, after_delete)
        })
        .await;
        assert_eq!(
            results.0,
            Some(None),
            "the first purchase has none before it"
        );
        assert_eq!(results.1, None, "the cooldown runs");
        assert_eq!(results.2, Some(Some(NOW_MS)));
        assert_eq!(
            results.3, None,
            "the released claim put the first purchase back"
        );
        assert_eq!(
            results.4,
            Some(None),
            "a released first purchase leaves no row"
        );
    }
}
