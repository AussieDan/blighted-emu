// SPDX-License-Identifier: AGPL-3.0-or-later
//! The economy of this sim's players (economy note `local-data/scratch-work/run15/economy`):
//! the trading post, player trades, the store flag and the season and reward calls.
//!
//! The trading post's orders are the world's (world database, [`nw_db::post_order`] and the
//! other order calls): a call becomes an [`EconomyJob`] that the loop runs in its own task, and
//! the [`EconomyResult`] comes back between ticks. The sim takes what a player gives (coins, the
//! units of a sell order or a buy order's sale) before the job and gives it back when the job
//! fails, is refused, or its player is gone (then as a delivery of the world database). What a
//! player receives always arrives as a delivery that its sim takes ([`SimWorld::economy_round`]
//! and after each own call), so a sale on another sim reaches the seller wherever it plays.
//!
//! A player trade runs in this sim: both players are near each other (retail refuses
//! `PlayersOutOfRange`). Each offer change shows in the trade state of both players; when both
//! confirmed their locked offers, the coins and items move at once and the move is audited.
//!
//! A shop purchase (3741) is one row of the shop offers ([`crate::shop`]): the sim checks the
//! row's rank, takes its coins, items and progression points, and gives its items or its
//! achievement at once, all or nothing. A row with a cooldown first claims the purchase in the
//! world database, so the cooldown holds on every sim; a claim whose purchase then fails is
//! given back.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::{ConnectionRef, ItemState};
use nw_common::time::unix_ms;
use nw_content::progression::Mastery;
use nw_db::{
    CharacterOrders, Completed, DbError, Delivery, DeliveryNotice, NewOrder, Order, OrderKind,
    OrderPage, OrderRefusal, OrderSearch, OrderStatus, PgPool, SavedItemRolls, TakenDelivery,
    TradeSide,
};
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::contracts::{
    CONTRACT_CANCELED_TYPE_INDEX, CONTRACT_CREATED_TYPE_INDEX, CONTRACT_EXPIRED_TYPE_INDEX,
    CONTRACT_RESOLVED_TYPE_INDEX, CONTRACTS_COMPONENT, ContractActionFailedMsg, ContractCall,
    ContractKind, ContractListMsg, ContractMessage, ContractNoticeMsg, ContractsFoundMsg,
    ContractsLookedUpMsg, CreateOrder, FullyCompletedMsg, NO_LIMIT, ParticipantQuery, SearchQuery,
    SearchResultsMsg, failure, open_filter, reason, role_filter,
};
use nw_messages::gear::record;
use nw_messages::items::{
    FullList, ItemRecord, SYNC_MOVED, SYNC_OWNED, SYNC_TRADING_POST_BOUGHT,
    SYNC_TRADING_POST_RETURNED, SYNC_TRADING_POST_TAKEN,
};
use nw_messages::p2p_trade::{
    OfferedStack, P2P_TRADE_COMPONENT, P2P_TRADE_STATE_KEY, P2PTradeState, TradeCall, TradeMessage,
    failure as trade_failure,
};
use nw_messages::seasons::{SeasonsCall, response};
use nw_messages::shop::ShopPurchaseCall;
use nw_messages::store::StoreCall;
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    BundleMember, EconomyCall, EconomyMessage, FacetHeader, InteractionRequest, MemberState,
    ServerMessage, put_is_in_store,
};
use uuid::Uuid;

use super::gear::{BagEntry, Gear, ItemRefusal, MAX_CONTAINER_SLOTS, MovedStack};
use super::quest_objects::with_coins;
use super::{Body, Player, SimWorld, gather_answer, send_now};
use crate::server::{saved_rolls, stored_values};
use crate::shop::ShopOffer;
use crate::trading_post::{
    ContractView, MS_PER_DAY, TradeItem, completion_cost, contract_of, next_page_token, order_kind,
    settlement_storage,
};

/// The interaction kind of a trading post (`InteractionUIActions` `OpenTradingPost`).
pub(super) const TRADING_POST_KIND: u8 = 21;
/// The status of an accepted interaction.
const INTERACTION_ACCEPTED: u8 = 1;
/// The status of a refused use of the trading post (Inferred: the refusal of a busy gather, 2).
const INTERACTION_REFUSED: u8 = 2;
/// The unnamed `u32` of retail's answer to a trading post interaction (Observed: all 14 answers
/// of 5 captures, at T3 and T5 trading posts).
const TRADING_POST_ANSWER_EXTRA: u32 = 0x5a3f_1797;
/// Most contracts one answer holds (retail pages hold at most 100).
const MAX_PAGE: u32 = 100;
/// The time a player waits after a search it was answered before its next one is answered,
/// ms. Retail (4 trading post captures, 159 searches): each of the 20 searches less than 1 s
/// after the player's last answered one was refused with `Throttled` (7), the latest 999 ms
/// after (`tradepost_post_sell_cancel_buyorder_sellorder` 415.5 s); every search 1.03 s or more
/// after was answered. A refused search does not start the wait again (since the last answered
/// search, 8 of the 20 came in the same tick as it).
const SEARCH_INTERVAL_MS: u64 = 1_000;
/// The `ContractType` value of a make-good contract, which this server has none of.
const MAKE_GOOD_KIND: u32 = 2;
/// The `PerkType` of a gem perk; an order item counts its gems (`m_gemPerkCount`).
const GEM_PERK_TYPE: &str = "Gem";
/// Metric: economy calls by `call` and `result`.
pub const METRIC_ECONOMY_CALLS: &str = "sim_economy_calls_total";
/// Metric: deliveries given to players.
pub const METRIC_DELIVERIES: &str = "sim_trading_post_deliveries_total";
/// Metric: economy jobs that failed in the database.
pub const METRIC_FAILED_JOBS: &str = "sim_economy_failed_jobs_total";
const CALL_LABEL: &str = "call";
const RESULT_LABEL: &str = "result";
const ACCEPTED: &str = "accepted";
const REFUSED: &str = "refused";

/// What the sim took from a player for a job, given back when the job does not go through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refund {
    /// Coins, hundredths.
    pub coins: u64,
    /// Items: id and quantity.
    pub items: Option<(String, u32)>,
    /// The own values of the stack taken ([`MovedStack::values`]); `None` for a plain item.
    pub values: Option<ItemState>,
}

impl Refund {
    const fn is_empty(&self) -> bool {
        self.coins == 0 && self.items.is_none()
    }

    /// The delivery that gives it back to `character` in the world database.
    fn delivery(&self, character: Uuid) -> Delivery {
        Delivery {
            character,
            order: None,
            notice: DeliveryNotice::None,
            coins: self.coins,
            item: self.items.clone(),
            completed_count: 0,
            request: None,
            rolls: self.rolls(),
        }
    }

    /// The own values of the stack taken as the database stores them.
    fn rolls(&self) -> SavedItemRolls {
        self.values.as_ref().map(saved_rolls).unwrap_or_default()
    }
}

/// Who a job answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// The player's connection.
    pub connection: ConnectionRef,
    /// The player's character.
    pub character: Uuid,
    /// The header of its call.
    pub header: FacetHeader,
    /// The request or lookup id of its call.
    pub request: [u8; RAW_ID_LEN],
}

/// A piece of economy work for the world database; the loop runs it with [`run_job`].
#[derive(Debug, Clone, PartialEq)]
pub enum EconomyJob {
    /// One search page.
    Search {
        /// Who asks.
        caller: Caller,
        /// The search.
        search: OrderSearch,
    },
    /// Orders by id (223).
    Lookup {
        /// Who asks.
        caller: Caller,
        /// The ids.
        ids: Vec<Uuid>,
    },
    /// A character's orders (2278).
    CharacterLookup {
        /// Who asks.
        caller: Caller,
        /// Which orders.
        wanted: CharacterOrders,
    },
    /// A new order.
    Post {
        /// Who posts.
        caller: Caller,
        /// The order.
        order: Box<NewOrder>,
        /// What the sim took for it.
        refund: Refund,
    },
    /// The first step of a completion: the order as it is.
    Inspect {
        /// Who completes.
        caller: Caller,
        /// The order.
        order: Uuid,
        /// The call.
        call: CompletionCall,
    },
    /// A completion of another player's order.
    Complete {
        /// Who completes.
        caller: Caller,
        /// The order.
        order: Uuid,
        /// Units.
        units: u32,
        /// The completer's trading tax.
        tax_rate: f64,
        /// What the sim took for it.
        refund: Refund,
    },
    /// A cancel of the caller's own order.
    Cancel {
        /// Who cancels.
        caller: Caller,
        /// The order.
        order: Uuid,
    },
    /// Take the deliveries of these characters.
    Take {
        /// The characters.
        characters: Vec<Uuid>,
    },
    /// Owe these deliveries again.
    Return {
        /// The deliveries.
        deliveries: Vec<Delivery>,
    },
    /// Audit one player trade.
    RecordTrade {
        /// One side.
        first: TradeSide,
        /// The other.
        second: TradeSide,
    },
    /// Claim a shop purchase whose offer has a cooldown.
    ClaimShop {
        /// The purchase.
        claim: ShopClaim,
    },
    /// Give back a claimed shop purchase that did not go through.
    ReleaseShop {
        /// The purchase.
        claim: ShopClaim,
        /// The purchase before it; `None` for none.
        previous_ms: Option<u64>,
    },
}

impl EconomyResult {
    /// The characters whose coins or items the answer moves in this sim (a buy or a cancel
    /// that the database took, the deliveries it gave out): their state is saved at once after
    /// it, so a sim that stops before its next save round cannot lose what the database gave.
    pub fn movers(&self) -> Vec<Uuid> {
        match self {
            Self::Posted { caller, .. }
            | Self::Completed { caller, .. }
            | Self::Canceled { caller, .. } => vec![caller.character],
            Self::Taken { characters, .. } => characters.clone(),
            Self::ShopClaimed { claim, .. } => vec![claim.character],
            Self::Searched { .. }
            | Self::Found { .. }
            | Self::Listed { .. }
            | Self::Inspected { .. } => Vec::new(),
        }
    }
}

impl EconomyJob {
    /// The characters whose coins or items this sim moved for the job before the database has
    /// it (a post, a buy, a cancel, a delivery taken, a player trade, a shop claim): their state
    /// is saved at once, so a sim that stops before its next save round cannot give the goods
    /// back to a player whose order or trade the database keeps.
    pub fn movers(&self) -> Vec<Uuid> {
        match self {
            Self::Post { caller, .. }
            | Self::Complete { caller, .. }
            | Self::Cancel { caller, .. } => {
                vec![caller.character]
            }
            Self::Take { characters } => characters.clone(),
            Self::RecordTrade { first, second } => vec![first.character, second.character],
            Self::ClaimShop { claim } | Self::ReleaseShop { claim, .. } => vec![claim.character],
            Self::Search { .. }
            | Self::Lookup { .. }
            | Self::CharacterLookup { .. }
            | Self::Inspect { .. }
            | Self::Return { .. } => Vec::new(),
        }
    }
}

/// A shop purchase whose offer has a cooldown, while its claim runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopClaim {
    /// The buyer's connection.
    pub connection: ConnectionRef,
    /// The buyer's character.
    pub character: Uuid,
    /// The offer: the CRC-32 of its lowercase `ConversionID`.
    pub conversion: u32,
    /// Its `ConversionID`.
    pub conversion_id: String,
    /// How many times it is bought.
    pub count: u32,
    /// The time of the purchase.
    pub now_ms: u64,
    /// The offer's cooldown.
    pub cooldown_ms: u64,
}

/// What the client's completion call asks, checked against the order it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionCall {
    /// The `ContractType` value the client names.
    pub kind: u32,
    /// Units.
    pub units: u32,
    /// The CRC-32 of the item id the client names; 0 for none.
    pub item_crc: u32,
}

/// The answer of a job.
#[derive(Debug)]
pub enum EconomyResult {
    /// A search page.
    Searched {
        /// Who asked.
        caller: Caller,
        /// The page.
        page: Result<OrderPage, DbError>,
        /// The matches skipped.
        start: u32,
    },
    /// Orders by id.
    Found {
        /// Who asked.
        caller: Caller,
        /// The orders.
        orders: Result<Vec<Order>, DbError>,
    },
    /// A character's orders.
    Listed {
        /// Who asked.
        caller: Caller,
        /// The orders.
        orders: Result<Vec<(Order, bool)>, DbError>,
    },
    /// A post.
    Posted {
        /// Who posted.
        caller: Caller,
        /// The order.
        posted: Result<Result<Order, OrderRefusal>, DbError>,
        /// What the sim took.
        refund: Refund,
    },
    /// The order of a completion's first step.
    Inspected {
        /// Who completes.
        caller: Caller,
        /// The order; `None` for no such order.
        order: Result<Option<Order>, DbError>,
        /// The call.
        call: CompletionCall,
    },
    /// A completion.
    Completed {
        /// Who completed.
        caller: Caller,
        /// The completion.
        completed: Result<Result<Completed, OrderRefusal>, DbError>,
        /// What the sim took.
        refund: Refund,
    },
    /// A cancel.
    Canceled {
        /// Who canceled.
        caller: Caller,
        /// The order.
        canceled: Result<Result<Order, OrderRefusal>, DbError>,
    },
    /// Deliveries taken.
    Taken {
        /// The characters asked for.
        characters: Vec<Uuid>,
        /// The deliveries.
        taken: Result<Vec<TakenDelivery>, DbError>,
    },
    /// A shop purchase claim.
    ShopClaimed {
        /// The purchase.
        claim: ShopClaim,
        /// `None` while the cooldown runs, else the purchase before (`None` for none).
        claimed: Result<Option<Option<u64>>, DbError>,
    },
}

/// Runs `job` on the world database `world`; `None` for a job without an answer (a return or a
/// trade audit, whose failure is logged here).
pub async fn run_job(world: &PgPool, job: EconomyJob) -> Option<EconomyResult> {
    Some(match job {
        EconomyJob::Search { caller, search } => EconomyResult::Searched {
            start: search.start,
            page: nw_db::search_orders(world, &search).await,
            caller,
        },
        EconomyJob::Lookup { caller, ids } => EconomyResult::Found {
            orders: nw_db::orders_by_id(world, &ids).await,
            caller,
        },
        EconomyJob::CharacterLookup { caller, wanted } => EconomyResult::Listed {
            orders: nw_db::character_orders(world, &wanted).await,
            caller,
        },
        EconomyJob::Post {
            caller,
            order,
            refund,
        } => EconomyResult::Posted {
            posted: nw_db::post_order(world, &order).await,
            caller,
            refund,
        },
        EconomyJob::Inspect {
            caller,
            order,
            call,
        } => EconomyResult::Inspected {
            order: nw_db::orders_by_id(world, &[order])
                .await
                .map(|orders| orders.into_iter().next()),
            caller,
            call,
        },
        EconomyJob::Complete {
            caller,
            order,
            units,
            tax_rate,
            refund,
        } => EconomyResult::Completed {
            completed: nw_db::complete_order(
                world,
                order,
                caller.character,
                units,
                tax_rate,
                Uuid::from_bytes(caller.request),
                &refund.rolls(),
                unix_ms(),
            )
            .await,
            caller,
            refund,
        },
        EconomyJob::Cancel { caller, order } => EconomyResult::Canceled {
            canceled: nw_db::cancel_order(world, order, caller.character, unix_ms()).await,
            caller,
        },
        EconomyJob::Take { characters } => EconomyResult::Taken {
            taken: nw_db::take_deliveries(world, &characters).await,
            characters,
        },
        EconomyJob::Return { deliveries } => {
            if let Err(error) = nw_db::return_deliveries(world, &deliveries).await {
                metrics::counter!(METRIC_FAILED_JOBS).increment(1);
                tracing::error!(%error, ?deliveries, "economy deliveries not returned");
            }
            return None;
        }
        EconomyJob::RecordTrade { first, second } => {
            if let Err(error) = nw_db::record_trade(world, &first, &second).await {
                metrics::counter!(METRIC_FAILED_JOBS).increment(1);
                tracing::error!(%error, "player trade not audited");
            }
            return None;
        }
        EconomyJob::ClaimShop { claim } => EconomyResult::ShopClaimed {
            claimed: nw_db::claim_shop_purchase(
                world,
                claim.character,
                &claim.conversion_id,
                claim.now_ms,
                claim.cooldown_ms,
            )
            .await,
            claim,
        },
        EconomyJob::ReleaseShop { claim, previous_ms } => {
            let released = nw_db::release_shop_purchase(
                world,
                claim.character,
                &claim.conversion_id,
                claim.now_ms,
                previous_ms,
            )
            .await;
            if let Err(error) = released {
                metrics::counter!(METRIC_FAILED_JOBS).increment(1);
                tracing::error!(%error, ?claim, "shop purchase claim not given back");
            }
            return None;
        }
    })
}

/// Why a shop purchase is refused; nothing changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShopRefusal {
    /// No offer has the id.
    UnknownOffer,
    /// A count of 0, or totals out of range.
    InvalidCount,
    /// The offer needs a paid entitlement, which this server never grants.
    NeedsEntitlement,
    /// The buyer's rank is below the offer's.
    RankTooLow,
    /// The buyer lacks coins.
    NotEnoughCoins,
    /// The buyer lacks the items the offer costs.
    NotEnoughItems,
    /// The buyer lacks the progression points the offer costs.
    NotEnoughPoints,
    /// The content does not have an item or a progression of the offer.
    UnknownContent,
    /// The bag cannot take the items.
    BagFull,
    /// The buyer has the achievement the offer gives.
    AlreadyOwned,
    /// The buyer bought the offer less than its cooldown ago.
    CooldownRunning,
    /// The buyer is gone, or has no bag yet.
    NoPlayer,
}

/// A shop purchase checked against its buyer: what the buyer has after it.
#[derive(Debug)]
struct PricedPurchase {
    /// The coins after it.
    coins: u64,
    /// The bag after it; `None` when the offer neither costs nor gives items.
    gear: Option<Gear>,
    /// Each progression it costs points of, and its points after it.
    points: Vec<(Mastery, u32)>,
    /// The achievement it gives.
    achievement: Option<String>,
}

/// One player's side of a trade.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TradeSideState {
    number: u64,
    entity: Entity,
    character: Uuid,
    stacks: Vec<OfferedStack>,
    coin: u64,
    locked: bool,
    confirmed: bool,
}

impl TradeSideState {
    fn new(number: u64, entity: Entity, character: Uuid) -> Self {
        Self {
            number,
            entity,
            character,
            stacks: Vec::new(),
            coin: 0,
            locked: false,
            confirmed: false,
        }
    }
}

/// One running trade.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TradeSession {
    sides: [TradeSideState; 2],
}

/// The economy state of this sim's players.
#[derive(Debug, Default)]
pub(super) struct Economy {
    /// The outpost of the trading post each player opened last, by player number.
    posts: HashMap<u64, u64>,
    /// When each player's last answered search came, Unix ms, by player number.
    searched_ms: HashMap<u64, u64>,
    /// Work for the loop.
    jobs: Vec<EconomyJob>,
    /// The characters whose deliveries a take is running for.
    taking: HashSet<Uuid>,
    /// The running trades, by the number of one of their players.
    trades: HashMap<u64, u64>,
    /// The trades by id.
    sessions: HashMap<u64, TradeSession>,
    /// The id of the next trade.
    next_trade: u64,
    /// The generic invites that wait for their answer.
    pub(super) invites: super::invites::Invites,
    /// The store purchases whose entitlement the loop saves next.
    pub(super) grants: Vec<super::store_purchase::StoreGrant>,
    /// The last store transaction number given.
    pub(super) last_transaction: u64,
}

impl Economy {
    /// Whether a search of player `number` at `now_ms` is answered ([`SEARCH_INTERVAL_MS`]);
    /// an answered one starts the wait again.
    fn may_search(&mut self, number: u64, now_ms: u64) -> bool {
        let waited = self
            .searched_ms
            .get(&number)
            .is_none_or(|last| now_ms.saturating_sub(*last) >= SEARCH_INTERVAL_MS);
        if waited {
            self.searched_ms.insert(number, now_ms);
        }
        waited
    }
}

fn economy(message: EconomyMessage) -> ServerMessage {
    ServerMessage::Economy(message)
}

fn contract_message(message: ContractMessage) -> ServerMessage {
    economy(EconomyMessage::Contract(Box::new(message)))
}

fn trade_message(message: TradeMessage) -> ServerMessage {
    economy(EconomyMessage::Trade(message))
}

fn action_failed(caller: &Caller, failure_reason: u32) -> ServerMessage {
    contract_message(ContractMessage::Failed(ContractActionFailedMsg {
        header: caller.header,
        request: caller.request,
        reason: failure_reason,
    }))
}

fn notice(type_index: u64, caller: &Caller, order: &Order) -> ServerMessage {
    contract_message(ContractMessage::Notice(ContractNoticeMsg {
        type_index,
        header: caller.header,
        request: caller.request,
        contract: contract_of(order, caller.character, ContractView::Notice),
    }))
}

/// The failure reason of a refused order call.
const fn refusal_reason(refusal: OrderRefusal) -> u32 {
    match refusal {
        OrderRefusal::NoSuchOrder | OrderRefusal::NotOpen => failure::ALREADY_RESOLVED,
        OrderRefusal::TooManyOrders => failure::MAXIMUM_CONTRACTS,
        OrderRefusal::NotEnoughUnits => failure::INSUFFICIENT_ITEMS,
        OrderRefusal::OwnOrder => failure::CREATOR_CANNOT_COMPLETE,
        OrderRefusal::NotCreator => failure::INVALID_REQUEST,
    }
}

pub(super) fn count_call(call: &'static str, accepted: bool) {
    metrics::counter!(
        METRIC_ECONOMY_CALLS,
        CALL_LABEL => call,
        RESULT_LABEL => if accepted { ACCEPTED } else { REFUSED }
    )
    .increment(1);
}

/// The text of a search field; `None` for an empty one (any value).
fn filter_text(text: &[u8]) -> Option<String> {
    (!text.is_empty()).then(|| String::from_utf8_lossy(text).into_owned())
}

/// The order search of `query`.
fn order_search(query: &SearchQuery, now_ms: u64) -> OrderSearch {
    OrderSearch {
        kind: order_kind(query.wanted_kind()),
        category: filter_text(&query.category),
        family: filter_text(&query.family),
        group: filter_text(&query.group),
        item_id: filter_text(&query.item),
        tier: (query.tier != NO_LIMIT).then_some(query.tier),
        outposts: query.outpost_ids(),
        perks: query
            .perks
            .iter()
            .map(|perk| lowercase_crc32(&String::from_utf8_lossy(perk)))
            .collect(),
        start: query.start,
        limit: u32::from(query.page_size).clamp(1, MAX_PAGE),
        now_ms,
    }
}

/// The orders of character `character` that a participant lookup asks for.
fn character_orders(character: Uuid, query: &ParticipantQuery) -> CharacterOrders {
    let limit = u32::from(query.page_size).clamp(1, MAX_PAGE);
    CharacterOrders {
        character,
        open: query.open_filter != open_filter::CLOSED,
        closed: query.open_filter != open_filter::OPEN,
        created: matches!(query.role_filter, role_filter::ALL | role_filter::CREATOR),
        completed: query.role_filter != role_filter::CREATOR,
        kinds: query
            .kinds
            .iter()
            .filter(|kind| **kind != MAKE_GOOD_KIND)
            .filter_map(|kind| ContractKind::of_value(*kind).map(order_kind))
            .collect(),
        start: query.page.saturating_mul(limit),
        limit,
    }
}

/// The open orders a world transfer check names: every open order the character created.
fn transfer_check_orders(character: Uuid) -> CharacterOrders {
    CharacterOrders {
        character,
        open: true,
        closed: false,
        created: true,
        completed: false,
        kinds: vec![OrderKind::Buy, OrderKind::Sell],
        start: 0,
        limit: MAX_PAGE,
    }
}

impl SimWorld {
    /// The economy work queued since the last call, for the loop.
    pub fn take_economy_jobs(&mut self) -> Vec<EconomyJob> {
        std::mem::take(&mut self.economy.jobs)
    }

    /// Asks for the deliveries of every player of this sim that no take runs for, and clears
    /// the invites that ran out.
    pub fn economy_round(&mut self) {
        self.expire_invites();
        let characters: Vec<Uuid> = self
            .by_connection
            .values()
            .filter_map(|entity| self.world.get::<Player>(*entity))
            .map(|player| player.character)
            .filter(|character| !self.economy.taking.contains(character))
            .collect();
        self.take_deliveries(characters);
    }

    fn take_deliveries(&mut self, characters: Vec<Uuid>) {
        let characters: Vec<Uuid> = characters
            .into_iter()
            .filter(|character| self.economy.taking.insert(*character))
            .collect();
        if !characters.is_empty() {
            self.economy.jobs.push(EconomyJob::Take { characters });
        }
    }

    /// Takes one economy call of the client of `connection`.
    pub fn economy_call(&mut self, connection: &ConnectionRef, call: EconomyCall) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        match call {
            EconomyCall::Contract(call) => self.contract_call(entity, call),
            EconomyCall::Trade(call) => self.trade_call(entity, &call),
            EconomyCall::Seasons(call) => self.seasons_call(entity, &call),
            EconomyCall::Store(call) => self.store_call(entity, &call),
            EconomyCall::Shop(call) => self.shop_purchase(entity, &call, unix_ms()),
            EconomyCall::Invite(call) => self.invite_call(entity, &call),
        }
    }

    /// The caller of a call with `header` and `request` by player `entity`.
    fn caller(
        &self,
        entity: Entity,
        header: FacetHeader,
        request: [u8; RAW_ID_LEN],
    ) -> Option<Caller> {
        let player = self.world.get::<Player>(entity)?;
        Some(Caller {
            connection: player.connection.clone(),
            character: player.character,
            header,
            request,
        })
    }

    fn reply(&mut self, caller: &Caller, messages: &[ServerMessage]) {
        send_now(&mut self.combat_messages, &caller.connection, messages);
    }

    fn contract_call(&mut self, entity: Entity, call: ContractCall) {
        let now_ms = unix_ms();
        match call {
            ContractCall::Search {
                header,
                query,
                lookup,
            } => {
                let Some(caller) = self.caller(entity, header, lookup) else {
                    return;
                };
                let number = self
                    .world
                    .get::<Player>(entity)
                    .map_or(0, |player| player.number);
                if !self.economy.may_search(number, now_ms) {
                    count_call("search", false);
                    return self.reply(&caller, &[action_failed(&caller, failure::THROTTLED)]);
                }
                count_call("search", true);
                self.economy.jobs.push(EconomyJob::Search {
                    caller,
                    search: order_search(&query, now_ms),
                });
            }
            ContractCall::Lookup {
                header,
                ids,
                lookup,
                ..
            } => {
                let Some(caller) = self.caller(entity, header, lookup) else {
                    return;
                };
                count_call("lookup", true);
                self.economy.jobs.push(EconomyJob::Lookup {
                    caller,
                    ids: ids.into_iter().map(Uuid::from_bytes).collect(),
                });
            }
            ContractCall::TransferCheck { header, lookup } => {
                let Some(caller) = self.caller(entity, header, lookup) else {
                    return;
                };
                count_call("transfer_check", true);
                let wanted = transfer_check_orders(caller.character);
                self.economy
                    .jobs
                    .push(EconomyJob::CharacterLookup { caller, wanted });
            }
            ContractCall::Create {
                header,
                order,
                request,
                ..
            } => self.create_order(entity, header, &order, request, now_ms),
            ContractCall::Complete {
                header,
                contract,
                request,
                kind,
                count,
                reason: call_reason,
                item,
                ..
            } => {
                let Some(caller) = self.caller(entity, header, request) else {
                    return;
                };
                let order = Uuid::from_bytes(contract);
                if call_reason == reason::CANCELED {
                    count_call("cancel", true);
                    self.economy.jobs.push(EconomyJob::Cancel { caller, order });
                } else if call_reason == reason::SUCCEEDED && count > 0 {
                    count_call("complete", true);
                    self.economy.jobs.push(EconomyJob::Inspect {
                        caller,
                        order,
                        call: CompletionCall {
                            kind,
                            units: count,
                            item_crc: item.item_crc,
                        },
                    });
                } else {
                    count_call("complete", false);
                    let refused = action_failed(&caller, failure::INVALID_REQUEST);
                    self.reply(&caller, &[refused]);
                }
            }
        }
    }

    /// Answers the participant lookup 2556 of player `entity`: its orders from the world
    /// database (2278).
    pub(super) fn lookup_contracts(
        &mut self,
        entity: Entity,
        header: FacetHeader,
        query: &ParticipantQuery,
        lookup: [u8; RAW_ID_LEN],
    ) {
        let Some(caller) = self.caller(entity, header, lookup) else {
            return;
        };
        count_call("participant_lookup", true);
        let wanted = character_orders(caller.character, query);
        self.economy
            .jobs
            .push(EconomyJob::CharacterLookup { caller, wanted });
    }

    /// Player `number` (`entity`) opens the trading post of `request`, whose name its client
    /// sends (`@TradingPostT5_MasterName`): accepted with the name and retail's `u32`
    /// ([`TRADING_POST_ANSWER_EXTRA`]); the post's settlement is the one whose capital is
    /// nearest to the player ([`crate::trading_post::Settlements::nearest`]). Far from every
    /// settlement the use is refused (status 2, Inferred as for a busy gather): a client cannot
    /// open the trading post from anywhere.
    pub(super) fn open_trading_post(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
        name: &[u8],
    ) {
        let outpost = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
            .and_then(|at| self.content.settlements.nearest(at.x, at.y));
        let Some(player) = self.world.get::<Player>(entity) else {
            return;
        };
        let connection = player.connection.clone();
        let Some(outpost) = outpost else {
            self.economy.posts.remove(&number);
            tracing::debug!(player = number, "trading post far from every settlement");
            let refusal = gather_answer(number, request, INTERACTION_REFUSED, &[]);
            send_now(&mut self.combat_messages, &connection, &[refusal]);
            return;
        };
        self.economy.posts.insert(number, outpost);
        let mut answer = gather_answer(number, request, INTERACTION_ACCEPTED, name);
        if let ServerMessage::InteractionValidated(validated) = &mut answer {
            validated.extra = TRADING_POST_ANSWER_EXTRA;
        }
        send_now(&mut self.combat_messages, &connection, &[answer]);
        tracing::debug!(player = number, outpost, "trading post opened");
    }

    /// A create call (2972) of player `entity`: checked here, its coins and items taken, then
    /// posted by a job.
    fn create_order(
        &mut self,
        entity: Entity,
        header: FacetHeader,
        order: &CreateOrder,
        request: [u8; RAW_ID_LEN],
        now_ms: u64,
    ) {
        let Some(caller) = self.caller(entity, header, request) else {
            return;
        };
        let refused = |world: &mut Self, failure_reason| {
            count_call("create", false);
            let message = action_failed(&caller, failure_reason);
            world.reply(&caller, &[message]);
        };
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return;
        };
        let Some(&outpost) = self.economy.posts.get(&number) else {
            return refused(self, failure::INVALID_OUTPOST);
        };
        let content = Arc::clone(&self.content);
        let rules = &content.trading_post;
        let Some(kind) = ContractKind::of_value(order.kind).map(order_kind) else {
            return refused(self, failure::INVALID_REQUEST);
        };
        if !rules.allows(order.days) {
            return refused(self, failure::INVALID_DURATION);
        }
        let Some(item) = content.trading.by_crc(order.item_crc).cloned() else {
            return refused(self, failure::INVALID_REQUEST);
        };
        let Some(total) = order.price.checked_mul(u64::from(order.count)) else {
            return refused(self, failure::INVALID_REQUEST);
        };
        if order.count == 0 || order.price == 0 {
            return refused(self, failure::INVALID_REQUEST);
        }
        let Some(fee) = rules.posting_fee(kind, order.days, total) else {
            return refused(self, failure::INVALID_REQUEST);
        };
        let tax_rate = rules.tax_at(outpost);
        let max_open = rules.max_open_orders;
        let named = Some(order.instance()).filter(|instance| *instance != [0; RAW_ID_LEN]);
        let refund = match self.take_for_order(entity, kind, &item, named, order.count, total, fee)
        {
            Ok(refund) => refund,
            Err(failure_reason) => return refused(self, failure_reason),
        };
        let order_item = match kind {
            OrderKind::Sell => {
                let kit = content.kits.item(&item.item_id);
                let values = refund.values.as_ref();
                let gear_score = values
                    .and_then(|values| values.gear_score)
                    .or_else(|| kit.map(|kit| kit.gear_score))
                    .unwrap_or_default();
                let perks: Vec<String> = values
                    .and_then(|values| values.perks.clone())
                    .or_else(|| kit.map(|kit| kit.perk_ids.clone()))
                    .unwrap_or_default();
                let gems = perks
                    .iter()
                    .filter(|perk| {
                        content
                            .item_rolls
                            .perks
                            .perk(perk)
                            .is_some_and(|perk| perk.perk_type == GEM_PERK_TYPE)
                    })
                    .count();
                item.order_item(
                    gear_score,
                    perks.iter().map(|perk| lowercase_crc32(perk)).collect(),
                    u32::try_from(gems).unwrap_or(u32::MAX),
                    SavedItemRolls {
                        gear_score: None,
                        ..refund.rolls()
                    },
                )
            }
            OrderKind::Buy => item.order_item(0, Vec::new(), 0, SavedItemRolls::default()),
        };
        count_call("create", true);
        self.economy.jobs.push(EconomyJob::Post {
            order: Box::new(NewOrder {
                id: Uuid::new_v4(),
                kind,
                creator: caller.character,
                outpost,
                item: order_item,
                price: order.price,
                quantity: order.count,
                tax_rate,
                created_ms: now_ms,
                expires_ms: now_ms.saturating_add(u64::from(order.days) * MS_PER_DAY),
                posting_fee: fee,
                max_open,
            }),
            caller,
            refund,
        });
    }

    /// Takes from player `entity` what a new order of `count` units of `item` worth `total`
    /// costs: the posting fee, and a buy order's total or a sell order's units (the stack of
    /// instance `named` when the call names one), with their own values. A locked or bound
    /// stack is not sold.
    #[allow(clippy::too_many_arguments, reason = "one order's costs")]
    fn take_for_order(
        &mut self,
        entity: Entity,
        kind: OrderKind,
        item: &TradeItem,
        named: Option<[u8; RAW_ID_LEN]>,
        count: u32,
        total: u64,
        fee: u64,
    ) -> Result<Refund, u32> {
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return Err(failure::SERVER_ERROR);
        };
        let coins = match kind {
            OrderKind::Buy => total.checked_add(fee).ok_or(failure::INSUFFICIENT_GOLD)?,
            OrderKind::Sell => fee,
        };
        if player.coins < coins {
            return Err(failure::INSUFFICIENT_GOLD);
        }
        let (items, values) = match kind {
            OrderKind::Sell => {
                let moved = player
                    .gear
                    .as_mut()
                    .ok_or(failure::INSUFFICIENT_ITEMS)?
                    .take_offered(&item.item_id, named, count, Some(SYNC_TRADING_POST_TAKEN))
                    .map_err(|_| failure::INSUFFICIENT_ITEMS)?;
                (Some((item.item_id.clone(), count)), moved.values)
            }
            OrderKind::Buy => (None, None),
        };
        if coins > 0 {
            player.coins -= coins;
            player.coins_changed = true;
        }
        Ok(Refund {
            coins,
            items,
            values,
        })
    }

    /// Gives `refund` back to the player of `caller`: at once when it plays here, else as a
    /// delivery of the world database.
    fn give_back(&mut self, caller: &Caller, refund: &Refund) {
        if refund.is_empty() {
            return;
        }
        let leftover = self
            .by_connection
            .get(&caller.connection)
            .copied()
            .filter(|entity| {
                self.world
                    .get::<Player>(*entity)
                    .is_some_and(|player| player.character == caller.character)
            })
            .map_or_else(
                || Some(refund.delivery(caller.character)),
                |entity| self.give(entity, refund.delivery(caller.character), None),
            );
        if let Some(leftover) = leftover {
            self.economy.jobs.push(EconomyJob::Return {
                deliveries: vec![leftover],
            });
        }
    }

    /// Gives `delivery`'s coins (under the wallet cap) and items to player `entity`: the items
    /// into storage `storage` when it names one, else into the bag; what does not fit comes
    /// back. The items of a completion show [`SYNC_TRADING_POST_BOUGHT`]; those given back (a
    /// cancel, an expiry, a refused call) [`SYNC_TRADING_POST_RETURNED`].
    fn give(
        &mut self,
        entity: Entity,
        delivery: Delivery,
        storage: Option<[u8; RAW_ID_LEN]>,
    ) -> Option<Delivery> {
        let wallet_cap = self.content.gameplay.wallet_cap;
        let content = Arc::clone(&self.content);
        let kit = delivery
            .item
            .as_ref()
            .and_then(|(item, _)| content.kits.item(item).cloned());
        let mut player = self.world.get_mut::<Player>(entity)?;
        let mut leftover = Delivery {
            coins: 0,
            item: None,
            ..delivery.clone()
        };
        if delivery.coins > 0 {
            let before = player.coins;
            player.coins = with_coins(before, delivery.coins, wallet_cap);
            player.coins_changed = true;
            leftover.coins = delivery.coins - (player.coins - before).min(delivery.coins);
        }
        if let Some((item, quantity)) = delivery.item {
            let reason = if delivery.completed_count > 0 {
                SYNC_TRADING_POST_BOUGHT
            } else {
                SYNC_TRADING_POST_RETURNED
            };
            // A stack with values of its own goes whole or waits whole (with its values); a bag
            // that fills part way with a plain item keeps what it took: only the rest waits.
            let values = (delivery.rolls != SavedItemRolls::default())
                .then(|| stored_values(&delivery.rolls));
            let given = match (&kit, player.gear.as_mut(), storage) {
                (Some(kit), Some(gear), Some(storage)) => {
                    let stored = match &values {
                        Some(values) => gear.store_with_values(
                            storage,
                            &content.kits,
                            kit,
                            quantity,
                            values,
                            reason,
                        ),
                        None => gear.store_new(storage, kit, quantity, reason),
                    };
                    u32::from(stored) * quantity
                }
                (Some(kit), Some(gear), None) => match &values {
                    Some(values) => {
                        let put =
                            gear.add_with_values(&content.kits, kit, quantity, values, reason);
                        u32::from(put) * quantity
                    }
                    None => {
                        let before = gear.bag_count(&item);
                        gear.add_as(kit, quantity, reason);
                        gear.bag_count(&item).saturating_sub(before).min(quantity)
                    }
                },
                _ => 0,
            };
            if given < quantity {
                let waiting = quantity - given;
                tracing::warn!(
                    character = %delivery.character,
                    item = %item,
                    waiting,
                    "a delivery's items do not fit; they wait"
                );
                leftover.item = Some((item, waiting));
            }
        }
        metrics::counter!(METRIC_DELIVERIES).increment(1);
        (leftover.coins > 0 || leftover.item.is_some()).then_some(leftover)
    }

    /// Takes the answer of an economy job.
    pub fn economy_done(&mut self, result: EconomyResult) {
        match result {
            EconomyResult::Searched {
                caller,
                page,
                start,
            } => self.searched(&caller, page, start),
            EconomyResult::Found { caller, orders } => match orders {
                Ok(orders) => {
                    let found = ContractsFoundMsg(ContractListMsg {
                        header: caller.header,
                        lookup: caller.request,
                        contracts: orders
                            .iter()
                            .map(|order| {
                                contract_of(order, caller.character, ContractView::LookedUp)
                            })
                            .collect(),
                    });
                    self.reply(&caller, &[contract_message(ContractMessage::Found(found))]);
                }
                Err(error) => self.job_failed(&caller, &error),
            },
            EconomyResult::Listed { caller, orders } => match orders {
                Ok(orders) => {
                    let listed = ContractsLookedUpMsg(ContractListMsg {
                        header: caller.header,
                        lookup: caller.request,
                        contracts: orders
                            .iter()
                            .map(|(order, _)| {
                                contract_of(order, caller.character, ContractView::Listed)
                            })
                            .collect(),
                    });
                    self.reply(
                        &caller,
                        &[contract_message(ContractMessage::LookedUp(listed))],
                    );
                }
                Err(error) => self.job_failed(&caller, &error),
            },
            EconomyResult::Posted {
                caller,
                posted,
                refund,
            } => match posted {
                Ok(Ok(order)) => {
                    let created = notice(CONTRACT_CREATED_TYPE_INDEX, &caller, &order);
                    self.reply(&caller, &[created]);
                }
                Ok(Err(refusal)) => {
                    self.give_back(&caller, &refund);
                    self.reply(&caller, &[action_failed(&caller, refusal_reason(refusal))]);
                }
                Err(error) => {
                    self.give_back(&caller, &refund);
                    self.job_failed(&caller, &error);
                }
            },
            EconomyResult::Inspected {
                caller,
                order,
                call,
            } => match order {
                Ok(Some(order)) => self.complete_inspected(&caller, &order, call),
                Ok(None) => {
                    let refused = action_failed(&caller, failure::ALREADY_RESOLVED);
                    self.reply(&caller, &[refused]);
                }
                Err(error) => self.job_failed(&caller, &error),
            },
            EconomyResult::Completed {
                caller,
                completed,
                refund,
            } => match completed {
                Ok(Ok(completed)) => {
                    let resolved = notice(CONTRACT_RESOLVED_TYPE_INDEX, &caller, &completed.order);
                    self.reply(&caller, &[resolved]);
                    self.take_deliveries(vec![caller.character]);
                }
                Ok(Err(refusal)) => {
                    self.give_back(&caller, &refund);
                    self.reply(&caller, &[action_failed(&caller, refusal_reason(refusal))]);
                }
                Err(error) => {
                    self.give_back(&caller, &refund);
                    self.job_failed(&caller, &error);
                }
            },
            EconomyResult::Canceled { caller, canceled } => match canceled {
                Ok(Ok(order)) => {
                    let messages = [
                        notice(CONTRACT_CANCELED_TYPE_INDEX, &caller, &order),
                        notice(CONTRACT_RESOLVED_TYPE_INDEX, &caller, &order),
                    ];
                    self.reply(&caller, &messages);
                    self.take_deliveries(vec![caller.character]);
                }
                Ok(Err(refusal)) => {
                    self.reply(&caller, &[action_failed(&caller, refusal_reason(refusal))]);
                }
                Err(error) => self.job_failed(&caller, &error),
            },
            EconomyResult::Taken { characters, taken } => {
                for character in &characters {
                    self.economy.taking.remove(character);
                }
                match taken {
                    Ok(taken) => self.deliver(taken),
                    Err(error) => {
                        metrics::counter!(METRIC_FAILED_JOBS).increment(1);
                        tracing::warn!(%error, "trading post deliveries not taken");
                    }
                }
            }
            EconomyResult::ShopClaimed { claim, claimed } => self.shop_claimed(claim, claimed),
        }
    }

    fn job_failed(&mut self, caller: &Caller, error: &DbError) {
        metrics::counter!(METRIC_FAILED_JOBS).increment(1);
        tracing::warn!(%error, character = %caller.character, "trading post call failed");
        self.reply(caller, &[action_failed(caller, failure::SERVER_ERROR)]);
    }

    fn searched(&mut self, caller: &Caller, page: Result<OrderPage, DbError>, start: u32) {
        let page = match page {
            Ok(page) => page,
            Err(error) => return self.job_failed(caller, &error),
        };
        let shown = u32::try_from(page.orders.len()).unwrap_or(u32::MAX);
        let more = start.saturating_add(shown) < page.total;
        let next_page = page
            .orders
            .last()
            .filter(|_| more)
            .map(next_page_token)
            .unwrap_or_default();
        let results = SearchResultsMsg {
            header: caller.header,
            lookup: caller.request,
            contracts: page
                .orders
                .iter()
                .map(|order| contract_of(order, caller.character, ContractView::Listed))
                .collect(),
            next_page,
            total: page.total,
        };
        self.reply(
            caller,
            &[contract_message(ContractMessage::SearchResults(results))],
        );
    }

    /// The second step of a completion: `order` as it is, checked against the client's `call`;
    /// the completer's coins (a sell order) or units (a buy order) are taken, then the
    /// completion is a job.
    fn complete_inspected(&mut self, caller: &Caller, order: &Order, call: CompletionCall) {
        let mismatch = order.status != OrderStatus::Available
            || ContractKind::of_value(call.kind).map(order_kind) != Some(order.kind)
            || (call.item_crc != 0 && call.item_crc != order.item.item_crc);
        let failure_reason = if order.creator == caller.character {
            Some(failure::CREATOR_CANNOT_COMPLETE)
        } else if mismatch {
            Some(failure::ALREADY_RESOLVED)
        } else if call.units > order.open {
            Some(failure::INSUFFICIENT_ITEMS)
        } else {
            None
        };
        if let Some(failure_reason) = failure_reason {
            return self.reply(caller, &[action_failed(caller, failure_reason)]);
        }
        let Some(entity) = self.by_connection.get(&caller.connection).copied() else {
            return;
        };
        let tax_rate = self.completer_rate(entity, order);
        let Some((coins, items)) = completion_cost(order, call.units, tax_rate) else {
            return self.reply(caller, &[action_failed(caller, failure::INVALID_REQUEST)]);
        };
        let taken = {
            let Some(mut player) = self.world.get_mut::<Player>(entity) else {
                return;
            };
            let paid = if player.coins < coins {
                Err(failure::INSUFFICIENT_GOLD)
            } else if let Some((item, units)) = &items {
                player
                    .gear
                    .as_mut()
                    .ok_or(failure::INSUFFICIENT_ITEMS)
                    .and_then(|gear| {
                        gear.take_offered(item, None, *units, Some(SYNC_TRADING_POST_TAKEN))
                            .map_err(|_| failure::INSUFFICIENT_ITEMS)
                    })
                    .map(|moved| moved.values)
            } else {
                Ok(None)
            };
            if paid.is_ok() && coins > 0 {
                player.coins -= coins;
                player.coins_changed = true;
            }
            paid
        };
        let values = match taken {
            Ok(values) => values,
            Err(failure_reason) => {
                return self.reply(caller, &[action_failed(caller, failure_reason)]);
            }
        };
        self.economy.jobs.push(EconomyJob::Complete {
            caller: caller.clone(),
            order: order.id,
            units: call.units,
            tax_rate,
            refund: Refund {
                coins,
                items,
                values,
            },
        });
    }

    /// The trading tax player `entity` pays as the completer of `order`: the tax of the trading
    /// post it opened. Retail (`buy-sell-TP` and `Hivesellandbuyorder`, both at one settlement):
    /// the tax a completer paid followed the completer, 2.35 % for one and 5 % for the other, 4
    /// of 4. A player without an opened trading post pays the order's rate (Inferred: the rate of
    /// the order's settlement).
    fn completer_rate(&self, entity: Entity, order: &Order) -> f64 {
        self.world
            .get::<Player>(entity)
            .and_then(|player| self.economy.posts.get(&player.number))
            .map_or(order.tax_rate, |outpost| {
                self.content.trading_post.tax_at(*outpost)
            })
    }

    /// Gives the taken deliveries to their players with their notices (2964 then 1117 for a
    /// fully completed order, 917 for an expired one); what is not given is owed again. Retail
    /// (`buy-sell-TP` 220.126 s and 224.298 s, `Hivesellandbuyorder` 258.115 s and 262.316 s): the
    /// 2964 shows the order as its last completion left it and repeats that completion's request;
    /// the 1117 of the proceeds shows it resolved, under a new request id. The count of a 2964
    /// after several completions (this server: the last one's units) is Unknown: retail fully
    /// completed each sample at once. The units of a filled buy order go to its creator's
    /// storage at the order's settlement ([`crate::world::gear::Gear::store_new`]); an order
    /// whose outpost is no settlement of the level (posted before settlements were named) fills
    /// the bag.
    fn deliver(&mut self, taken: Vec<TakenDelivery>) {
        let present: HashMap<Uuid, (Entity, u64, ConnectionRef)> = self
            .by_connection
            .values()
            .filter_map(|entity| {
                let player = self.world.get::<Player>(*entity)?;
                Some((
                    player.character,
                    (*entity, player.number, player.connection.clone()),
                ))
            })
            .collect();
        let mut returned = Vec::new();
        let expiry_batch = *Uuid::new_v4().as_bytes();
        for TakenDelivery { delivery, order } in taken {
            let Some((entity, number, connection)) = present.get(&delivery.character).cloned()
            else {
                returned.push(delivery);
                continue;
            };
            let header = FacetHeader::of(CONTRACTS_COMPONENT, number);
            let character = delivery.character;
            let messages: Vec<ServerMessage> = match (&order, delivery.notice) {
                (Some(order), DeliveryNotice::FullyCompleted) => {
                    let request = delivery
                        .request
                        .map_or([0; RAW_ID_LEN], |id| *id.as_bytes());
                    vec![
                        contract_message(ContractMessage::FullyCompleted(FullyCompletedMsg {
                            header,
                            request,
                            contract: contract_of(
                                &completed_snapshot(order),
                                character,
                                ContractView::Notice,
                            ),
                            count: delivery.completed_count,
                        })),
                        contract_message(ContractMessage::Notice(ContractNoticeMsg {
                            type_index: CONTRACT_RESOLVED_TYPE_INDEX,
                            header,
                            request: *Uuid::new_v4().as_bytes(),
                            contract: contract_of(order, character, ContractView::Notice),
                        })),
                    ]
                }
                (Some(order), DeliveryNotice::Expired) => {
                    vec![contract_message(ContractMessage::Notice(
                        ContractNoticeMsg {
                            type_index: CONTRACT_EXPIRED_TYPE_INDEX,
                            header,
                            request: expiry_batch,
                            contract: contract_of(order, character, ContractView::Notice),
                        },
                    ))]
                }
                _ => Vec::new(),
            };
            let storage = order
                .as_ref()
                .filter(|order| {
                    order.kind == OrderKind::Buy
                        && order.creator == character
                        && delivery.completed_count > 0
                        && self.content.settlements.contains(order.outpost)
                })
                .map(|order| settlement_storage(order.outpost));
            if let Some(leftover) = self.give(entity, delivery, storage) {
                returned.push(leftover);
            }
            send_now(&mut self.combat_messages, &connection, &messages);
        }
        if !returned.is_empty() {
            self.economy.jobs.push(EconomyJob::Return {
                deliveries: returned,
            });
        }
    }

    /// Player `number` left this sim: its invite and its trade end, and its trading post and
    /// last search are forgotten.
    pub(super) fn economy_left(&mut self, number: u64) {
        self.economy.posts.remove(&number);
        self.economy.searched_ms.remove(&number);
        self.invites_left(number);
        self.end_trade(number, trade_failure::TRADE_CANCELED);
    }

    /// Whether player `number` trades.
    pub(super) fn trading(&self, number: u64) -> bool {
        self.economy.trades.contains_key(&number)
    }

    fn store_call(&mut self, entity: Entity, call: &StoreCall) {
        match call {
            StoreCall::SetIsInStore { in_store, .. } => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    put_is_in_store(&mut player.extra_members, *in_store);
                }
                count_call("store_flag", true);
            }
            StoreCall::Purchase { header, purchase } => {
                self.store_purchase(entity, *header, purchase);
            }
            StoreCall::ClaimEntitlement { entitlement, .. } => {
                // No entitlement is granted on this server, so none can be claimed.
                count_call("claim_entitlement", false);
                tracing::info!(
                    entitlement,
                    "entitlement claim refused: none is granted here"
                );
            }
            StoreCall::PurchaseReward { item, cost, .. } => {
                // The reward track state of every player is empty: no reward is offered.
                count_call("reward_track_purchase", false);
                tracing::info!(item, cost, "reward track purchase refused: no track here");
            }
            StoreCall::ResetRewardTrack { .. } => {
                count_call("reward_track_reset", false);
                tracing::info!("reward track reset refused: no track here");
            }
        }
    }

    /// A shop purchase (3741) of player `entity` at `now_ms`: bought at once, or after the
    /// claim of an offer with a cooldown. Retail answers with the changes of the bag and the
    /// currencies only, so a refused purchase changes nothing and is logged.
    fn shop_purchase(&mut self, entity: Entity, call: &ShopPurchaseCall, now_ms: u64) {
        let content = Arc::clone(&self.content);
        let priced = content
            .shops
            .by_crc(call.conversion)
            .ok_or(ShopRefusal::UnknownOffer)
            .and_then(|offer| {
                self.price_purchase(entity, offer, call.count)
                    .map(|priced| (offer, priced))
            });
        let (offer, priced) = match priced {
            Ok(found) => found,
            Err(refusal) => return shop_refused(call.conversion, refusal),
        };
        let Some(cooldown_ms) = offer.cooldown_ms else {
            self.pay_purchase(entity, priced);
            count_call("shop", true);
            return;
        };
        let Some(player) = self.world.get::<Player>(entity) else {
            return;
        };
        let claim = ShopClaim {
            connection: player.connection.clone(),
            character: player.character,
            conversion: call.conversion,
            conversion_id: offer.conversion_id.clone(),
            count: call.count,
            now_ms,
            cooldown_ms,
        };
        self.economy.jobs.push(EconomyJob::ClaimShop { claim });
    }

    /// The answer of a shop claim: the purchase goes through when the claim was stored and the
    /// buyer still can pay; a stored claim whose purchase fails is given back.
    fn shop_claimed(&mut self, claim: ShopClaim, claimed: Result<Option<Option<u64>>, DbError>) {
        let previous_ms = match claimed {
            Ok(Some(previous_ms)) => previous_ms,
            Ok(None) => {
                return shop_refused(claim.conversion, ShopRefusal::CooldownRunning);
            }
            Err(error) => {
                metrics::counter!(METRIC_FAILED_JOBS).increment(1);
                tracing::warn!(%error, offer = %claim.conversion_id, "shop claim failed");
                return count_call("shop", false);
            }
        };
        let content = Arc::clone(&self.content);
        let entity = self
            .by_connection
            .get(&claim.connection)
            .copied()
            .filter(|entity| {
                self.world
                    .get::<Player>(*entity)
                    .is_some_and(|player| player.character == claim.character)
            });
        let priced = match (entity, content.shops.by_crc(claim.conversion)) {
            (Some(entity), Some(offer)) => self
                .price_purchase(entity, offer, claim.count)
                .map(|priced| (entity, priced)),
            (None, _) => Err(ShopRefusal::NoPlayer),
            (_, None) => Err(ShopRefusal::UnknownOffer),
        };
        match priced {
            Ok((entity, priced)) => {
                self.pay_purchase(entity, priced);
                count_call("shop", true);
            }
            Err(refusal) => {
                shop_refused(claim.conversion, refusal);
                self.economy
                    .jobs
                    .push(EconomyJob::ReleaseShop { claim, previous_ms });
            }
        }
    }

    /// Checks `count` purchases of `offer` by player `entity`: what it has after them, or why
    /// it cannot buy them.
    fn price_purchase(
        &self,
        entity: Entity,
        offer: &ShopOffer,
        count: u32,
    ) -> Result<PricedPurchase, ShopRefusal> {
        if offer.needs_entitlement {
            return Err(ShopRefusal::NeedsEntitlement);
        }
        if count == 0 {
            return Err(ShopRefusal::InvalidCount);
        }
        let player = self
            .world
            .get::<Player>(entity)
            .ok_or(ShopRefusal::NoPlayer)?;
        let progression = &self.content.progression;
        if let Some((rank_progression, rank)) = &offer.required_rank
            && player.masteries.rank(progression, rank_progression) < *rank
        {
            return Err(ShopRefusal::RankTooLow);
        }
        if let Some(achievement) = &offer.achievement
            && player
                .flow
                .has_achievement(&self.achievement_rules, achievement)
        {
            return Err(ShopRefusal::AlreadyOwned);
        }
        let coins = offer
            .coins
            .checked_mul(u64::from(count))
            .ok_or(ShopRefusal::InvalidCount)?;
        let coins = player
            .coins
            .checked_sub(coins)
            .ok_or(ShopRefusal::NotEnoughCoins)?;
        let points = offer
            .progression_costs
            .iter()
            .map(|cost| {
                let mastery = progression
                    .categorical(&cost.progression)
                    .ok_or(ShopRefusal::UnknownContent)?;
                let total = cost
                    .points
                    .checked_mul(count)
                    .ok_or(ShopRefusal::InvalidCount)?;
                let left = player
                    .masteries
                    .experience(mastery)
                    .checked_sub(total)
                    .ok_or(ShopRefusal::NotEnoughPoints)?;
                Ok((mastery.clone(), left))
            })
            .collect::<Result<Vec<_>, ShopRefusal>>()?;
        let gear = if offer.item_cost.is_some() || offer.item.is_some() {
            let mut gear = player.gear.clone().ok_or(ShopRefusal::NoPlayer)?;
            if let Some((item, quantity)) = &offer.item_cost {
                let total = quantity
                    .checked_mul(count)
                    .ok_or(ShopRefusal::InvalidCount)?;
                if !gear.take(item, total) {
                    return Err(ShopRefusal::NotEnoughItems);
                }
            }
            if let Some((item, quantity)) = &offer.item {
                let kit = self
                    .content
                    .kits
                    .item(item)
                    .ok_or(ShopRefusal::UnknownContent)?;
                let total = quantity
                    .checked_mul(count)
                    .ok_or(ShopRefusal::InvalidCount)?;
                if !gear.add(kit, total) {
                    return Err(ShopRefusal::BagFull);
                }
            }
            Some(gear)
        } else {
            None
        };
        Ok(PricedPurchase {
            coins,
            gear,
            points,
            achievement: offer.achievement.clone(),
        })
    }

    /// Carries out a priced purchase of player `entity`.
    fn pay_purchase(&mut self, entity: Entity, priced: PricedPurchase) {
        let rules = &self.achievement_rules;
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        if player.coins != priced.coins {
            player.coins = priced.coins;
            player.coins_changed = true;
        }
        if let Some(gear) = priced.gear {
            player.gear = Some(gear);
        }
        for (mastery, left) in &priced.points {
            player.masteries.set(mastery, *left);
        }
        if let Some(achievement) = &priced.achievement {
            player.flow.set_achievement(rules, achievement);
        }
    }

    /// Answers a season call: this server has no season rewards or activity cards (the season
    /// states of its players are empty), so each call is refused in the retail answer form.
    fn seasons_call(&mut self, entity: Entity, call: &SeasonsCall) {
        let Some(connection) = self
            .world
            .get::<Player>(entity)
            .map(|player| player.connection.clone())
        else {
            return;
        };
        count_call("seasons", false);
        let answer = economy(EconomyMessage::Seasons(call.answer(response::FAILURE)));
        send_now(&mut self.combat_messages, &connection, &[answer]);
    }

    /// Starts a trade between players `first` and `second` of this sim (the accepted generic
    /// invite of activity `trade`); `false` when either is missing or trades already.
    pub(super) fn start_trade(&mut self, first: Entity, second: Entity) -> bool {
        let sides = [first, second].map(|entity| {
            self.world
                .get::<Player>(entity)
                .map(|player| TradeSideState::new(player.number, entity, player.character))
        });
        let [Some(a), Some(b)] = sides else {
            return false;
        };
        if a.number == b.number
            || self.economy.trades.contains_key(&a.number)
            || self.economy.trades.contains_key(&b.number)
        {
            return false;
        }
        let id = self.economy.next_trade;
        self.economy.next_trade += 1;
        self.economy.trades.insert(a.number, id);
        self.economy.trades.insert(b.number, id);
        let started = [(&a, &b), (&b, &a)].map(|(me, other)| {
            (
                me.entity,
                trade_message(TradeMessage::Started {
                    header: FacetHeader::of(P2P_TRADE_COMPONENT, me.number),
                    other: *other.character.as_bytes(),
                }),
            )
        });
        self.economy
            .sessions
            .insert(id, TradeSession { sides: [a, b] });
        for (entity, message) in started {
            self.send_to(entity, &[message]);
            self.put_trade_state(entity, reset_state());
        }
        true
    }

    pub(crate) fn send_to(&mut self, entity: Entity, messages: &[ServerMessage]) {
        let Some(connection) = self
            .world
            .get::<Player>(entity)
            .map(|player| player.connection.clone())
        else {
            return;
        };
        send_now(&mut self.combat_messages, &connection, messages);
    }

    fn put_trade_state(&mut self, entity: Entity, state: P2PTradeState) {
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.extra_members.push(BundleMember {
                component: P2P_TRADE_STATE_KEY,
                state: MemberState::P2PTrade(state),
            });
        }
    }

    /// The session of player `number` and its side index.
    fn trade_of(&self, number: u64) -> Option<(u64, usize)> {
        let id = *self.economy.trades.get(&number)?;
        let session = self.economy.sessions.get(&id)?;
        let side = session
            .sides
            .iter()
            .position(|side| side.number == number)?;
        Some((id, side))
    }

    fn trade_call(&mut self, entity: Entity, call: &TradeCall) {
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return;
        };
        let header = call.header();
        let Some((id, side)) = self.trade_of(number) else {
            if let TradeCall::Cancel { .. } = call {
                return;
            }
            return self.trade_response(entity, call, header, trade_failure::TRADE_STATE_MISMATCH);
        };
        let failure_value = match call {
            TradeCall::UpdateItems { stacks, .. } => self.offer_items(id, side, stacks),
            TradeCall::UpdateCoin { coin, .. } => self.offer_coin(id, side, *coin),
            TradeCall::LockIn { .. } => self.lock_offer(id, side, true),
            TradeCall::CancelLockIn { .. } => self.lock_offer(id, side, false),
            TradeCall::Confirm { .. } => self.confirm_trade(id, side),
            TradeCall::Cancel { reason, .. } => {
                self.end_trade(number, *reason);
                return;
            }
        };
        self.trade_response(entity, call, header, failure_value);
        if failure_value == trade_failure::NONE
            && matches!(call, TradeCall::Confirm { .. })
            && self
                .economy
                .sessions
                .get(&id)
                .is_some_and(|session| session.sides.iter().all(|side| side.confirmed))
        {
            self.execute_trade(id);
        }
    }

    fn trade_response(
        &mut self,
        entity: Entity,
        call: &TradeCall,
        header: FacetHeader,
        failure_value: u32,
    ) {
        let call_type = call.type_index();
        let Some(type_index) = TradeMessage::response_type(call_type) else {
            return;
        };
        count_call("trade", failure_value == trade_failure::NONE);
        let response = trade_message(TradeMessage::Response {
            type_index,
            header,
            failure: failure_value,
        });
        self.send_to(entity, &[response]);
    }

    /// The records of `stacks` in the bag of player `entity`; `None` when one is not there,
    /// holds fewer, or is bound.
    fn offered_records(&self, entity: Entity, stacks: &[OfferedStack]) -> Option<Vec<ItemRecord>> {
        let gear = self.world.get::<Player>(entity)?.gear.as_ref()?;
        let mut seen = HashSet::new();
        stacks
            .iter()
            .map(|stack| {
                if !seen.insert(stack.instance) {
                    return None;
                }
                let (slot, item) = (0..MAX_CONTAINER_SLOTS).find_map(|slot| {
                    let slot = u32::try_from(slot).ok()?;
                    let (_, item) = gear.bag_item(slot)?;
                    (item.instance == stack.instance).then_some((slot, item))
                })?;
                let quantity = u32::try_from(stack.quantity).ok()?;
                if quantity == 0 || quantity > item.quantity || item.bound {
                    return None;
                }
                let mut offered = record(item, SYNC_OWNED, u16::try_from(slot).ok());
                offered.quantity = quantity;
                Some(offered)
            })
            .collect()
    }

    fn offer_items(&mut self, id: u64, side: usize, stacks: &[OfferedStack]) -> u32 {
        let Some(session) = self.economy.sessions.get(&id) else {
            return trade_failure::TRADE_STATE_MISMATCH;
        };
        if session.sides[side].locked {
            return trade_failure::TRADE_STATE_MISMATCH;
        }
        let me = session.sides[side].entity;
        let other = session.sides[1 - side].entity;
        let Some(records) = self.offered_records(me, stacks) else {
            return trade_failure::INVALID_ITEMS;
        };
        if let Some(session) = self.economy.sessions.get_mut(&id) {
            session.sides[side].stacks = stacks.to_vec();
            unconfirm(session);
        }
        let list = FullList::first(records);
        self.put_trade_state(
            me,
            P2PTradeState {
                own_items: Some(list.clone()),
                ..P2PTradeState::default()
            },
        );
        self.put_trade_state(
            other,
            P2PTradeState {
                other_items: Some(list),
                ..P2PTradeState::default()
            },
        );
        trade_failure::NONE
    }

    fn offer_coin(&mut self, id: u64, side: usize, coin: u64) -> u32 {
        let Some(session) = self.economy.sessions.get(&id) else {
            return trade_failure::TRADE_STATE_MISMATCH;
        };
        if session.sides[side].locked {
            return trade_failure::TRADE_STATE_MISMATCH;
        }
        let me = session.sides[side].entity;
        let other = session.sides[1 - side].entity;
        let has = self
            .world
            .get::<Player>(me)
            .is_some_and(|player| player.coins >= coin);
        if !has {
            return trade_failure::INVALID_COIN_AMOUNT;
        }
        if let Some(session) = self.economy.sessions.get_mut(&id) {
            session.sides[side].coin = coin;
            unconfirm(session);
        }
        self.put_trade_state(
            me,
            P2PTradeState {
                own_coin: Some(coin),
                ..P2PTradeState::default()
            },
        );
        self.put_trade_state(
            other,
            P2PTradeState {
                other_coin: Some(coin),
                ..P2PTradeState::default()
            },
        );
        trade_failure::NONE
    }

    fn lock_offer(&mut self, id: u64, side: usize, locked: bool) -> u32 {
        let Some(session) = self.economy.sessions.get_mut(&id) else {
            return trade_failure::TRADE_STATE_MISMATCH;
        };
        session.sides[side].locked = locked;
        unconfirm(session);
        let me = session.sides[side].entity;
        let other = session.sides[1 - side].entity;
        self.put_trade_state(
            me,
            P2PTradeState {
                own_locked: Some(locked),
                ..P2PTradeState::default()
            },
        );
        self.put_trade_state(
            other,
            P2PTradeState {
                other_locked: Some(locked),
                ..P2PTradeState::default()
            },
        );
        trade_failure::NONE
    }

    fn confirm_trade(&mut self, id: u64, side: usize) -> u32 {
        let Some(session) = self.economy.sessions.get_mut(&id) else {
            return trade_failure::TRADE_STATE_MISMATCH;
        };
        if !session.sides.iter().all(|side| side.locked) {
            return trade_failure::BOTH_PLAYERS_NOT_LOCKED_IN;
        }
        if session.sides[side].confirmed {
            return trade_failure::TRADE_ALREADY_CONFIRMED;
        }
        session.sides[side].confirmed = true;
        let other = session.sides[1 - side].entity;
        let other_number = session.sides[1 - side].number;
        let confirmed = trade_message(TradeMessage::OtherConfirmed {
            header: FacetHeader::of(P2P_TRADE_COMPONENT, other_number),
        });
        self.send_to(other, &[confirmed]);
        trade_failure::NONE
    }

    /// Swaps the locked offers of trade `id`: each side's coins and stacks go to the other at
    /// once, or nothing moves and the trade ends with the reason.
    fn execute_trade(&mut self, id: u64) {
        let Some(session) = self.economy.sessions.get(&id).cloned() else {
            return;
        };
        let [a, b] = &session.sides;
        let outcome = self.swap(a, b);
        let reason_value = match &outcome {
            Ok(_) => trade_failure::NONE,
            Err(failure_value) => *failure_value,
        };
        if let Ok((first, second)) = outcome {
            self.economy
                .jobs
                .push(EconomyJob::RecordTrade { first, second });
        }
        self.end_trade(a.number, reason_value);
    }

    /// Moves `a`'s offer to `b` and `b`'s to `a`; the audit sides, or why nothing moved. Each
    /// stack goes whole, with its own values (gear score, perks, durability, bind state, look).
    /// A stack no longer offered as it was, a locked or bound one, too few coins, a full wallet
    /// or a bag without room for the other's offer moves nothing: every stack taken goes back
    /// to its owner.
    fn swap(
        &mut self,
        a: &TradeSideState,
        b: &TradeSideState,
    ) -> Result<(TradeSide, TradeSide), u32> {
        let wallet_cap = self.content.gameplay.wallet_cap;
        let a_items = self.offered_items(a)?;
        let b_items = self.offered_items(b)?;
        for (side, gains) in [(a, b.coin), (b, a.coin)] {
            let player = self
                .world
                .get::<Player>(side.entity)
                .ok_or(trade_failure::FAILED_TO_REMOVE)?;
            if player.coins < side.coin {
                return Err(trade_failure::FAILED_TO_REMOVE);
            }
            let after = player.coins - side.coin;
            if with_coins(after, gains, wallet_cap) != after.saturating_add(gains) {
                return Err(trade_failure::CANNOT_RECEIVE_ITEMS);
            }
        }
        let a_moved = self.take_offers(a, &a_items)?;
        let b_moved = match self.take_offers(b, &b_items) {
            Ok(moved) => moved,
            Err(failure_value) => {
                self.put_stacks(a.entity, a_moved);
                return Err(failure_value);
            }
        };
        if !self.has_room(b.entity, &a_moved) || !self.has_room(a.entity, &b_moved) {
            self.put_stacks(a.entity, a_moved);
            self.put_stacks(b.entity, b_moved);
            return Err(trade_failure::CANNOT_RECEIVE_ITEMS);
        }
        for (side, gains) in [(a, b.coin), (b, a.coin)] {
            if let Some(mut player) = self.world.get_mut::<Player>(side.entity) {
                player.coins = (player.coins - side.coin).saturating_add(gains);
                player.coins_changed = true;
            }
        }
        let audit = |side: &TradeSideState, moved: &[MovedStack]| TradeSide {
            character: side.character,
            coins: side.coin,
            items: moved
                .iter()
                .map(|stack| (stack.item_id.clone(), stack.quantity))
                .collect(),
        };
        let sides = (audit(a, &a_moved), audit(b, &b_moved));
        self.put_stacks(b.entity, a_moved);
        self.put_stacks(a.entity, b_moved);
        Ok(sides)
    }

    /// Takes each of `items` (item id, instance, quantity) out of `side`'s bag; on a refusal the
    /// stacks taken go back and nothing is taken.
    fn take_offers(
        &mut self,
        side: &TradeSideState,
        items: &[(String, [u8; RAW_ID_LEN], u32)],
    ) -> Result<Vec<MovedStack>, u32> {
        let mut moved = Vec::with_capacity(items.len());
        for (item, instance, quantity) in items {
            let taken = self
                .world
                .get_mut::<Player>(side.entity)
                .and_then(|mut player| {
                    let gear = player.gear.as_mut()?;
                    Some(gear.take_offered(item, Some(*instance), *quantity, None))
                })
                .unwrap_or(Err(ItemRefusal::NoSuchItem));
            match taken {
                Ok(stack) => moved.push(stack),
                Err(refusal) => {
                    self.put_stacks(side.entity, moved);
                    return Err(match refusal {
                        ItemRefusal::Locked | ItemRefusal::Bound => trade_failure::INVALID_ITEMS,
                        _ => trade_failure::FAILED_TO_REMOVE,
                    });
                }
            }
        }
        Ok(moved)
    }

    /// Whether the bag of player `entity` takes every one of `stacks` at once (nothing always
    /// fits; an item without a master row never does).
    pub(super) fn has_room(&self, entity: Entity, stacks: &[MovedStack]) -> bool {
        let entries: Option<Vec<BagEntry<'_>>> = stacks
            .iter()
            .map(|stack| {
                let item = self.content.kits.item(&stack.item_id)?;
                Some((item, stack.quantity, stack.values.is_some()))
            })
            .collect();
        stacks.is_empty()
            || entries.is_some_and(|entries| {
                self.world
                    .get::<Player>(entity)
                    .and_then(|player| player.gear.as_ref())
                    .is_some_and(|gear| gear.has_room_for(&entries))
            })
    }

    /// Puts `stacks` into the bag of player `entity`, each with its own values. Callers check
    /// [`SimWorld::has_room`] first, or give back stacks just taken from that bag.
    fn put_stacks(&mut self, entity: Entity, stacks: Vec<MovedStack>) {
        let content = Arc::clone(&self.content);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let Some(gear) = player.gear.as_mut() else {
            return;
        };
        for stack in stacks {
            let put = content.kits.item(&stack.item_id).is_some_and(|item| {
                gear.put_moved(
                    &content.kits,
                    item,
                    stack.quantity,
                    stack.values.as_ref(),
                    SYNC_MOVED,
                )
            });
            if !put {
                tracing::error!(item = %stack.item_id, quantity = stack.quantity, "a traded stack did not fit after the room check");
            }
        }
    }

    /// The item id, instance id and quantity of each stack `side` offers, still in its bag.
    fn offered_items(
        &self,
        side: &TradeSideState,
    ) -> Result<Vec<(String, [u8; RAW_ID_LEN], u32)>, u32> {
        if side.stacks.is_empty() {
            return Ok(Vec::new());
        }
        let gear = self
            .world
            .get::<Player>(side.entity)
            .and_then(|player| player.gear.as_ref())
            .ok_or(trade_failure::FAILED_TO_REMOVE)?;
        side.stacks
            .iter()
            .map(|stack| {
                let quantity =
                    u32::try_from(stack.quantity).map_err(|_| trade_failure::INVALID_ITEMS)?;
                (0..MAX_CONTAINER_SLOTS)
                    .find_map(|slot| {
                        let slot = u32::try_from(slot).ok()?;
                        let (item, record) = gear.bag_item(slot)?;
                        (record.instance == stack.instance && record.quantity >= quantity)
                            .then(|| (item.to_owned(), stack.instance, quantity))
                    })
                    .ok_or(trade_failure::FAILED_TO_REMOVE)
            })
            .collect()
    }

    /// Ends the trade of player `number` with `reason_value`: both players get 2179 and a reset
    /// trade state.
    fn end_trade(&mut self, number: u64, reason_value: u32) {
        let Some(id) = self.economy.trades.get(&number).copied() else {
            return;
        };
        let Some(session) = self.economy.sessions.remove(&id) else {
            return;
        };
        for side in &session.sides {
            self.economy.trades.remove(&side.number);
            let ended = trade_message(TradeMessage::Ended {
                header: FacetHeader::of(P2P_TRADE_COMPONENT, side.number),
                reason: reason_value,
            });
            self.send_to(side.entity, &[ended]);
            self.put_trade_state(side.entity, reset_state());
        }
    }
}

/// `resolved`, a fully completed order its creator took the proceeds of, as its last completion
/// left it: completed, one version before the take ([`nw_db::take_deliveries`]).
fn completed_snapshot(resolved: &Order) -> Order {
    Order {
        status: OrderStatus::Completed,
        version: resolved.version.saturating_sub(1),
        ..resolved.clone()
    }
}

/// Logs and counts a refused shop purchase of offer `conversion`.
fn shop_refused(conversion: u32, refusal: ShopRefusal) {
    count_call("shop", false);
    tracing::info!(conversion, ?refusal, "shop purchase refused");
}

/// An offer change unlocks nothing but ends both confirmations.
fn unconfirm(session: &mut TradeSession) {
    for side in &mut session.sides {
        side.confirmed = false;
    }
}

/// The trade state retail sends when a trade ends (`trading_between_players`: both offers
/// empty, no coin, neither locked).
fn reset_state() -> P2PTradeState {
    P2PTradeState {
        own_items: Some(FullList::first(Vec::new())),
        own_coin: Some(0),
        own_locked: Some(false),
        other_items: Some(FullList::first(Vec::new())),
        other_coin: None,
        other_locked: Some(false),
        can_receive: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player_content::PlayerContent;
    use crate::shop::ShopCatalog;
    use crate::trading_post::{Settlement, Settlements};
    use crate::world::gear;
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, world_with_spawns};
    use nw_content::generated::ItemCurrencyConversionData;
    use nw_content::progression::{Progression, ProgressionSource};
    use nw_messages::contracts::Participant;

    #[test]
    fn the_jobs_and_answers_that_move_goods_name_their_players_for_a_save() {
        let buyer = Uuid::from_u128(1);
        let seller = Uuid::from_u128(2);
        let caller = Caller {
            connection: crate::world::tests::connection(1),
            character: buyer,
            header: FacetHeader::of(0, 0),
            request: [0; RAW_ID_LEN],
        };
        let side = |character| TradeSide {
            character,
            coins: 100,
            items: Vec::new(),
        };
        assert_eq!(
            EconomyJob::RecordTrade {
                first: side(buyer),
                second: side(seller)
            }
            .movers(),
            [buyer, seller]
        );
        assert_eq!(
            EconomyJob::Take {
                characters: vec![seller]
            }
            .movers(),
            [seller]
        );
        assert_eq!(
            EconomyJob::Cancel {
                caller: caller.clone(),
                order: Uuid::from_u128(3),
            }
            .movers(),
            [buyer]
        );
        assert!(
            EconomyJob::Lookup {
                caller: caller.clone(),
                ids: Vec::new(),
            }
            .movers()
            .is_empty(),
            "a look-up moves nothing"
        );
        assert_eq!(
            EconomyResult::Taken {
                characters: vec![seller],
                taken: Ok(Vec::new()),
            }
            .movers(),
            [seller]
        );
    }

    #[test]
    fn a_search_query_becomes_an_order_search() {
        let query = SearchQuery {
            first: 1,
            start: 0,
            page_size: 500,
            action: 0,
            word_a: 0,
            byte_b: 1,
            word_c: 0,
            category: b"Resources".to_vec(),
            family: Vec::new(),
            group: Vec::new(),
            item: b"BeeswaxT1".to_vec(),
            byte_d: 0,
            tier: NO_LIMIT,
            outposts: vec![nw_messages::contracts::outpost_name(9)],
            float_e: -1.0,
            pairs: Vec::new(),
            bounds: [NO_LIMIT; nw_messages::contracts::SEARCH_BOUNDS],
            perks: vec![b"PerkID_Amulet_LoggingLuck".to_vec()],
            lists: [Vec::new(), Vec::new()],
            more_bounds: [NO_LIMIT; nw_messages::contracts::SEARCH_MORE_BOUNDS],
            perk_logic: 1,
            word_f: 1,
            word_g: 0,
        };
        let search = order_search(&query, 5);
        assert_eq!(search.kind, OrderKind::Sell);
        assert_eq!(search.category.as_deref(), Some("Resources"));
        assert_eq!(search.family, None);
        assert_eq!(search.item_id.as_deref(), Some("BeeswaxT1"));
        assert_eq!(search.tier, None);
        assert_eq!(search.outposts, vec![9]);
        assert_eq!(
            search.perks,
            vec![lowercase_crc32("PerkID_Amulet_LoggingLuck")]
        );
        assert_eq!(search.limit, MAX_PAGE, "a page holds at most 100 contracts");
    }

    /// The retail first lookup (`newchar-retail-2026-09-18`): the player's open sell orders,
    /// ten per page.
    #[test]
    fn a_participant_lookup_names_the_players_orders() {
        let character = Uuid::from_u128(7);
        let query = ParticipantQuery {
            participant: Participant {
                id: [1; RAW_ID_LEN],
                kind: 0,
            },
            open_filter: open_filter::OPEN,
            role_filter: role_filter::CREATOR,
            page: 1,
            page_size: 10,
            kinds: vec![1, MAKE_GOOD_KIND],
            tail: vec![9],
        };
        let wanted = character_orders(character, &query);
        assert_eq!(wanted.character, character);
        assert!(wanted.open && !wanted.closed);
        assert!(wanted.created && !wanted.completed);
        assert_eq!(wanted.kinds, vec![OrderKind::Sell]);
        assert_eq!((wanted.start, wanted.limit), (10, 10));
        let all = ParticipantQuery {
            open_filter: open_filter::ALL,
            role_filter: role_filter::ALL,
            ..query
        };
        let wanted = character_orders(character, &all);
        assert!(wanted.open && wanted.closed && wanted.created && wanted.completed);
    }

    #[test]
    fn a_search_within_a_second_of_the_last_answered_one_is_throttled() {
        /// Retail `tradepost_post_sell_cancel_buyorder_sellorder`: answered at 414.65 s,
        /// refused 999 ms later; answered searches 1.03 s apart.
        const FIRST_MS: u64 = 1_781_362_637_778;
        const REFUSED_AFTER_MS: u64 = 999;
        const ANSWERED_AFTER_MS: u64 = 1_030;
        let mut economy = Economy::default();
        const PLAYER: u64 = 1;
        const OTHER: u64 = 2;
        assert!(economy.may_search(PLAYER, FIRST_MS));
        assert!(!economy.may_search(PLAYER, FIRST_MS), "the same tick");
        assert!(!economy.may_search(PLAYER, FIRST_MS + REFUSED_AFTER_MS));
        assert!(
            economy.may_search(OTHER, FIRST_MS),
            "each player waits alone"
        );
        assert!(economy.may_search(PLAYER, FIRST_MS + ANSWERED_AFTER_MS));
        assert!(
            !economy.may_search(PLAYER, FIRST_MS + ANSWERED_AFTER_MS + REFUSED_AFTER_MS),
            "an answered search starts the wait again"
        );
    }

    #[test]
    fn refusals_have_failure_reasons() {
        assert_eq!(
            refusal_reason(OrderRefusal::TooManyOrders),
            failure::MAXIMUM_CONTRACTS
        );
        assert_eq!(
            refusal_reason(OrderRefusal::OwnOrder),
            failure::CREATOR_CANNOT_COMPLETE
        );
        assert_eq!(
            refusal_reason(OrderRefusal::NotOpen),
            failure::ALREADY_RESOLVED
        );
    }

    #[test]
    fn a_refund_without_its_player_is_a_delivery() {
        let refund = Refund {
            coins: 500,
            items: Some(("BeeswaxT1".to_owned(), 2)),
            values: None,
        };
        let delivery = refund.delivery(Uuid::from_u128(3));
        assert_eq!(delivery.order, None);
        assert_eq!(delivery.coins, 500);
        assert_eq!(delivery.item, Some(("BeeswaxT1".to_owned(), 2)));
        assert!(Refund::default().is_empty());
    }

    #[test]
    fn the_reset_trade_state_has_the_retail_bytes() {
        // Retail `trading_between_players` 31478 ms, the own state at a trade's end; this
        // server's list version is its first-state version.
        let mut writer = nw_messages::wire::Writer::new();
        reset_state().write(&mut writer).expect("write");
        let bytes = writer.finish();
        assert_eq!(&bytes[..2], &[0x01, 0x2f]);
        let state = P2PTradeState::read(&mut nw_messages::wire::Reader::new(&bytes)).expect("read");
        assert_eq!(state, reset_state());
    }

    #[test]
    fn transfer_checks_ask_for_every_open_own_order() {
        let wanted = transfer_check_orders(Uuid::from_u128(4));
        assert!(wanted.open && wanted.created && !wanted.closed && !wanted.completed);
        assert_eq!(wanted.kinds.len(), 2);
    }

    const BUYER: u64 = 1;
    /// The war camp currency of the captured purchases.
    const TOKENS: &str = "Battle_Token";
    /// The test cap of the tokens.
    const TOKEN_CAP: i64 = 5_000;
    const COINS: u64 = 1_000;
    const TOKENS_HELD: u32 = 50;
    const PRICE_COINS: i64 = 300;
    const PRICE_TOKENS: i64 = 20;
    const COOLDOWN_SECONDS: i64 = 86_400;
    const PREVIOUS_MS: u64 = 5;

    fn offer_row(conversion_id: &str) -> ItemCurrencyConversionData {
        ItemCurrencyConversionData {
            achievement_id: None,
            bought: 1,
            buy_categorical_progression_cost: Some(PRICE_TOKENS),
            buy_cooldown_seconds: None,
            buy_currency_cost: Some(PRICE_COINS),
            buy_currency_item_cost: None,
            buy_currency_item_name: None,
            buy_progression2_cost: None,
            buy_progression2_id: None,
            buy_progression3_cost: None,
            buy_progression3_id: None,
            categorical_progression_id: Some(TOKENS.to_owned()),
            conversion_id: conversion_id.to_owned(),
            display_order: None,
            entitlement_id: None,
            in_contracts: None,
            item_id: None,
            item_qty: None,
            perk1: None,
            perk2: None,
            perk3: None,
            perk4: None,
            rank_check_categorical_progression_id: Some(TOKENS.to_owned()),
            required_categorical_progression_rank: Some(0),
            sell_azoth_cost: None,
            sell_categorical_progression_cost: None,
            sell_currency_cost: None,
            sold: 0,
        }
    }

    /// A world with the shop offers of `rows` and one buyer with [`COINS`] and [`TOKENS_HELD`].
    fn shop_world(rows: &[ItemCurrencyConversionData]) -> (SimWorld, Entity) {
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(PlayerContent {
            progression: Progression::default().with_progressions([ProgressionSource {
                id: TOKENS.to_owned(),
                max_level: 0,
                auto_rank_up: false,
                ranks: vec![(0, TOKEN_CAP)],
            }]),
            shops: ShopCatalog::of(rows),
            ..PlayerContent::default()
        });
        world
            .admit(&admit_at(BUYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.player_by_number(BUYER).expect("buyer");
        let content = Arc::clone(&world.content);
        let tokens = content.progression.categorical(TOKENS).expect("tokens");
        {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            player.coins = COINS;
            player.masteries.set(tokens, TOKENS_HELD);
        }
        (world, entity)
    }

    fn purchase(conversion_id: &str, count: u32) -> ShopPurchaseCall {
        ShopPurchaseCall {
            header: FacetHeader::of(0, BUYER),
            conversion: lowercase_crc32(conversion_id),
            count,
        }
    }

    /// The buyer's coins and tokens.
    fn wallet(world: &SimWorld, entity: Entity) -> (u64, u32) {
        let tokens = world
            .content
            .progression
            .categorical(TOKENS)
            .expect("tokens");
        let player = world.world.get::<Player>(entity).expect("player");
        (player.coins, player.masteries.experience(tokens))
    }

    #[test]
    fn a_shop_purchase_takes_its_price_times_the_count_or_nothing() {
        let (mut world, entity) = shop_world(&[offer_row("PotionCleanseFTW")]);
        world.shop_purchase(entity, &purchase("PotionCleanseFTW", 2), NOW);
        assert_eq!(wallet(&world, entity), (400, 10));
        world.shop_purchase(entity, &purchase("PotionCleanseFTW", 1), NOW);
        assert_eq!(
            wallet(&world, entity),
            (400, 10),
            "too few tokens: nothing changes"
        );
        world.shop_purchase(entity, &purchase("NoSuchOffer", 1), NOW);
        world.shop_purchase(entity, &purchase("PotionCleanseFTW", 0), NOW);
        assert_eq!(wallet(&world, entity), (400, 10));
        assert!(
            world.take_economy_jobs().is_empty(),
            "no cooldown, no claim"
        );
    }

    #[test]
    fn a_shop_refuses_a_paid_offer_and_a_rank_the_buyer_lacks() {
        let rows = [
            ItemCurrencyConversionData {
                entitlement_id: Some("PaidEntitlement".to_owned()),
                ..offer_row("Paid")
            },
            ItemCurrencyConversionData {
                required_categorical_progression_rank: Some(1),
                ..offer_row("Ranked")
            },
        ];
        let (mut world, entity) = shop_world(&rows);
        world.shop_purchase(entity, &purchase("Paid", 1), NOW);
        world.shop_purchase(entity, &purchase("Ranked", 1), NOW);
        assert_eq!(wallet(&world, entity), (COINS, TOKENS_HELD));
    }

    #[test]
    fn an_offer_with_a_cooldown_is_bought_after_its_claim() {
        let rows = [ItemCurrencyConversionData {
            buy_cooldown_seconds: Some(COOLDOWN_SECONDS),
            ..offer_row("Daily")
        }];
        let (mut world, entity) = shop_world(&rows);
        world.shop_purchase(entity, &purchase("Daily", 1), NOW);
        assert_eq!(
            wallet(&world, entity),
            (COINS, TOKENS_HELD),
            "nothing before the claim"
        );
        let jobs = world.take_economy_jobs();
        let [EconomyJob::ClaimShop { claim }] = jobs.as_slice() else {
            panic!("one claim: {jobs:?}");
        };
        assert_eq!(claim.cooldown_ms, 86_400_000);
        assert_eq!(claim.conversion_id, "Daily");
        world.shop_claimed(claim.clone(), Ok(None));
        assert_eq!(
            wallet(&world, entity),
            (COINS, TOKENS_HELD),
            "the cooldown runs"
        );
        world.shop_claimed(claim.clone(), Ok(Some(None)));
        assert_eq!(wallet(&world, entity), (700, 30));
        world.world.get_mut::<Player>(entity).expect("player").coins = 0;
        world.shop_claimed(claim.clone(), Ok(Some(Some(PREVIOUS_MS))));
        assert_eq!(
            world.take_economy_jobs(),
            vec![EconomyJob::ReleaseShop {
                claim: claim.clone(),
                previous_ms: Some(PREVIOUS_MS),
            }],
            "a claim whose purchase fails is given back"
        );
    }

    const SELLER: u64 = 2;
    const OFFERED_COINS: u64 = 250;

    /// Player `number` (`entity`) makes the trade call `call` builds.
    fn trade(
        world: &mut SimWorld,
        entity: Entity,
        number: u64,
        call: fn(FacetHeader) -> TradeCall,
    ) {
        world.trade_call(entity, &call(FacetHeader::of(P2P_TRADE_COMPONENT, number)));
    }

    #[test]
    fn a_confirmed_trade_moves_the_locked_offers_at_once() {
        let (mut world, buyer) = shop_world(&[]);
        world
            .admit(&admit_at(SELLER, 1.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let seller = world.player_by_number(SELLER).expect("seller");
        assert!(world.start_trade(buyer, seller));
        assert!(!world.start_trade(buyer, seller), "one trade at a time");
        world.trade_call(
            buyer,
            &TradeCall::UpdateCoin {
                header: FacetHeader::of(P2P_TRADE_COMPONENT, BUYER),
                coin: OFFERED_COINS,
            },
        );
        trade(&mut world, buyer, BUYER, |header| TradeCall::Confirm {
            header,
        });
        assert!(
            world.trading(BUYER),
            "a confirm before both locked changes nothing"
        );
        trade(&mut world, buyer, BUYER, |header| TradeCall::LockIn {
            header,
        });
        trade(&mut world, seller, SELLER, |header| TradeCall::LockIn {
            header,
        });
        trade(&mut world, buyer, BUYER, |header| TradeCall::Confirm {
            header,
        });
        assert_eq!(wallet(&world, buyer).0, COINS, "one confirm moves nothing");
        trade(&mut world, seller, SELLER, |header| TradeCall::Confirm {
            header,
        });
        assert!(
            !world.trading(BUYER) && !world.trading(SELLER),
            "the trade ended"
        );
        assert_eq!(wallet(&world, buyer).0, COINS - OFFERED_COINS);
        let seller_coins = world.world.get::<Player>(seller).expect("seller").coins;
        assert_eq!(seller_coins, OFFERED_COINS);
        assert!(
            world
                .take_economy_jobs()
                .iter()
                .any(|job| matches!(job, EconomyJob::RecordTrade { .. })),
            "the trade is audited"
        );
    }

    /// The trading post test players: the seller carries [`STACK`] flint.
    pub(super) const SELLER_NUMBER: u64 = 11;
    pub(super) const BUYER_NUMBER: u64 = 12;
    pub(super) const STACK: u32 = 10;
    pub(super) const COINS_HELD: u64 = 1_000_000;
    /// The settlement of the retail `buy-sell-TP` orders.
    pub(super) const OUTPOST: u64 = 0xb325_909b_3cc6_434f;
    /// Another settlement (retail Morrowtide), far from the test players.
    const FAR_OUTPOST: u64 = 0x63e5_ce00_1ef4_6668;
    const FAR_METRES: f32 = 5_000.0;

    /// Content whose one tradable item is flint, and whose trading post at [`OUTPOST`] taxes
    /// `outpost_tax`.
    fn trading_content(outpost_tax: f64) -> PlayerContent {
        PlayerContent {
            kits: gear::tests::kits_with(&[]),
            trading: crate::trading_post::TradingCatalog::of([TradeItem {
                item_id: gear::tests::FLINT.to_owned(),
                category: "Resources".to_owned(),
                family: "RawResources".to_owned(),
                group: "Flint".to_owned(),
                tier: 1,
                required_level: None,
            }]),
            trading_post: crate::trading_post::TradingPostConfig {
                outposts: vec![crate::trading_post::OutpostTax {
                    outpost: OUTPOST,
                    trading_tax: outpost_tax,
                }],
                ..crate::trading_post::TradingPostConfig::default()
            },
            settlements: Settlements::of(vec![
                Settlement {
                    id: OUTPOST,
                    x: 0.0,
                    y: 0.0,
                },
                Settlement {
                    id: FAR_OUTPOST,
                    x: FAR_METRES,
                    y: FAR_METRES,
                },
            ]),
            ..PlayerContent::default()
        }
    }

    /// Two players at the trading post of [`OUTPOST`] (tax `outpost_tax`) with
    /// [`COINS_HELD`] each; the seller carries [`STACK`] flint and nothing was sent yet.
    pub(super) fn traders(outpost_tax: f64) -> (SimWorld, Entity, Entity) {
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(trading_content(outpost_tax));
        let mut entities = Vec::new();
        for number in [SELLER_NUMBER, BUYER_NUMBER] {
            let mut admit = admit_at(number, 0.0, true);
            admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
            world.admit(&admit, NOW, SERVER_TIME).expect("admit");
            let entity = world.player_by_number(number).expect("player");
            world.world.get_mut::<Player>(entity).expect("player").coins = COINS_HELD;
            world.economy.posts.insert(number, OUTPOST);
            entities.push(entity);
        }
        let flint = world
            .content
            .kits
            .item(gear::tests::FLINT)
            .cloned()
            .expect("flint");
        for (entity, quantity) in [(entities[0], STACK), (entities[1], 0)] {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            let gear = player.gear.as_mut().expect("gear");
            let held = gear.bag_count(gear::tests::FLINT);
            assert!(gear.take(gear::tests::FLINT, held));
            assert!(quantity == 0 || gear.add(&flint, quantity));
        }
        let _ = std::mem::take(&mut world.combat_messages);
        (world, entities[0], entities[1])
    }

    /// The seller of [`traders`] gets a sword with values of its own; its bag slot and instance.
    fn worn_out_sword(world: &mut SimWorld, seller: Entity) -> (u32, [u8; RAW_ID_LEN]) {
        let content = Arc::clone(&world.content);
        let sword = content.kits.item(gear::tests::SWORD).expect("sword");
        // Not bound: the test kit binds its items on pickup, and a bound item does not trade.
        let values = ItemState {
            gear_score: Some(OWN_GEAR_SCORE),
            durability: Some(OWN_DURABILITY),
            bound: Some(false),
            ..ItemState::default()
        };
        let mut player = world.world.get_mut::<Player>(seller).expect("seller");
        let gear = player.gear.as_mut().expect("gear");
        assert!(gear.add_with_values(&content.kits, sword, 1, &values, SYNC_MOVED));
        gear.bag_stacks()
            .into_iter()
            .rev()
            .find(|(_, item, _, _)| item == gear::tests::SWORD)
            .map(|(slot, _, instance, _)| (slot, instance))
            .expect("the sword in the bag")
    }

    /// The gear score and durability of each `item_id` stack in the bag of `entity`.
    fn bag_values(world: &SimWorld, entity: Entity, item_id: &str) -> Vec<(u32, u32)> {
        let gear = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .expect("gear");
        (0..u32::try_from(MAX_CONTAINER_SLOTS).expect("slots"))
            .filter_map(|slot| gear.bag_item(slot))
            .filter(|(item, _)| *item == item_id)
            .map(|(_, record)| (record.gear_score, record.durability))
            .collect()
    }

    const OWN_GEAR_SCORE: u32 = 512;
    const OWN_DURABILITY: u32 = 9;

    /// Both sides lock in and confirm the offers of trade between `seller` and `buyer`.
    fn trade_through(world: &mut SimWorld, seller: Entity, buyer: Entity) {
        for (entity, number) in [(seller, SELLER_NUMBER), (buyer, BUYER_NUMBER)] {
            trade(world, entity, number, |header| TradeCall::LockIn { header });
        }
        for (entity, number) in [(seller, SELLER_NUMBER), (buyer, BUYER_NUMBER)] {
            trade(world, entity, number, |header| TradeCall::Confirm {
                header,
            });
        }
    }

    #[test]
    fn a_traded_stack_keeps_its_own_values() {
        let (mut world, seller, buyer) = traders(0.0);
        let (_, sword) = worn_out_sword(&mut world, seller);
        assert!(world.start_trade(seller, buyer));
        world.trade_call(
            seller,
            &TradeCall::UpdateItems {
                header: FacetHeader::of(P2P_TRADE_COMPONENT, SELLER_NUMBER),
                stacks: vec![OfferedStack {
                    instance: sword,
                    quantity: 1,
                }],
            },
        );
        trade_through(&mut world, seller, buyer);
        assert!(!world.trading(BUYER_NUMBER), "the trade ended");
        assert_eq!(
            bag_values(&world, buyer, gear::tests::SWORD),
            [(OWN_GEAR_SCORE, OWN_DURABILITY)],
            "the buyer gets the worn sword, not a new one"
        );
        assert!(
            !bag_values(&world, seller, gear::tests::SWORD)
                .contains(&(OWN_GEAR_SCORE, OWN_DURABILITY))
        );
    }

    #[test]
    fn a_trade_into_a_full_bag_moves_nothing() {
        let (mut world, seller, buyer) = traders(0.0);
        let (_, sword) = worn_out_sword(&mut world, seller);
        {
            let content = Arc::clone(&world.content);
            let staff = content.kits.item(gear::tests::STAFF).expect("staff");
            let mut player = world.world.get_mut::<Player>(buyer).expect("buyer");
            let gear = player.gear.as_mut().expect("gear");
            while gear.has_room_for(&[(staff, 1, true)]) {
                assert!(gear.add_with_values(
                    &content.kits,
                    staff,
                    1,
                    &ItemState::default(),
                    SYNC_MOVED
                ));
            }
        }
        assert!(world.start_trade(seller, buyer));
        world.trade_call(
            seller,
            &TradeCall::UpdateItems {
                header: FacetHeader::of(P2P_TRADE_COMPONENT, SELLER_NUMBER),
                stacks: vec![OfferedStack {
                    instance: sword,
                    quantity: 1,
                }],
            },
        );
        trade_through(&mut world, seller, buyer);
        assert!(!world.trading(SELLER_NUMBER), "the trade ended, refused");
        assert_eq!(
            bag_values(&world, seller, gear::tests::SWORD)
                .iter()
                .filter(|values| **values == (OWN_GEAR_SCORE, OWN_DURABILITY))
                .count(),
            1,
            "the seller keeps its sword, with its values"
        );
        assert!(bag_values(&world, buyer, gear::tests::SWORD).is_empty());
    }

    /// A player opening a trading post trades at the nearest settlement; a filled buy order's
    /// units go to the storage of the order's settlement, with no bag change (retail
    /// `buy-sell-TP` 328.900 s, `Hivesellandbuyorder` 401.71 s).
    #[test]
    fn a_trading_post_trades_at_the_nearest_settlement_and_buy_orders_fill_its_storage() {
        const UNITS: u32 = 3;
        let (mut world, _, buyer) = traders(crate::trading_post::RETAIL_TRADING_TAX);
        world.economy.posts.clear();
        world.open_trading_post(
            buyer,
            BUYER_NUMBER,
            InteractionRequest {
                target: [7; RAW_ID_LEN],
                target_entity: 7,
                interaction: 1,
                kind: TRADING_POST_KIND,
                gatherable: None,
            },
            b"@TradingPostT5_MasterName",
        );
        assert_eq!(world.economy.posts.get(&BUYER_NUMBER), Some(&OUTPOST));
        let creator = world.world.get::<Player>(buyer).expect("buyer").character;
        let item = world
            .content
            .trading
            .by_id(gear::tests::FLINT)
            .expect("tradable")
            .order_item(0, Vec::new(), 0, SavedItemRolls::default());
        let filled = Order {
            id: Uuid::from_u128(5),
            kind: OrderKind::Buy,
            status: OrderStatus::Resolved,
            version: 3,
            creator,
            outpost: OUTPOST,
            item,
            price: 100,
            posted: UNITS,
            open: 0,
            end: nw_db::OrderEnd::Succeeded,
            tax_rate: crate::trading_post::RETAIL_TRADING_TAX,
            created_ms: NOW,
            expires_ms: NOW + MS_PER_DAY,
            completed_ms: Some(NOW),
            updated_ms: NOW,
        };
        let units = |order: Order| TakenDelivery {
            delivery: Delivery {
                character: creator,
                order: Some(order.id),
                notice: DeliveryNotice::FullyCompleted,
                coins: 0,
                item: Some((gear::tests::FLINT.to_owned(), UNITS)),
                completed_count: UNITS,
                request: None,

                rolls: SavedItemRolls::default(),
            },
            order: Some(order),
        };
        world.deliver(vec![units(filled.clone())]);
        let held = |world: &SimWorld| {
            let player = world.world.get::<Player>(buyer).expect("buyer");
            let gear = player.gear.as_ref().expect("gear");
            (
                gear.bag_count(gear::tests::FLINT),
                gear.stored_totals().get(gear::tests::FLINT).copied(),
                gear.stored_items(&settlement_storage(OUTPOST))
                    .map(|items| items.len()),
            )
        };
        assert_eq!(held(&world), (0, Some(UNITS), Some(1)));
        // An order of before the settlement ids (a territory number) fills the bag.
        world.deliver(vec![units(Order {
            outpost: 2,
            ..filled
        })]);
        assert_eq!(held(&world), (UNITS, Some(UNITS), Some(1)));
        assert!(world.take_economy_jobs().is_empty(), "nothing waits");
    }

    /// A delivery into a bag that fills part way: the bag keeps what it took, only the rest
    /// waits (nothing is given twice).
    #[test]
    fn a_delivery_into_a_bag_that_fills_waits_only_for_the_rest() {
        const SINGLE: &str = "TestSingleT1";
        const UNITS: u32 = 600;
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(PlayerContent {
            kits: gear::tests::kits_with_stacks(&[(SINGLE, "Resource", Some(1))]),
            ..PlayerContent::default()
        });
        let mut admit = admit_at(BUYER_NUMBER, 0.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let entity = world.player_by_number(BUYER_NUMBER).expect("player");
        let character = world.world.get::<Player>(entity).expect("player").character;
        let left = world
            .give(
                entity,
                Delivery {
                    character,
                    order: None,
                    notice: DeliveryNotice::None,
                    coins: 0,
                    item: Some((SINGLE.to_owned(), UNITS)),
                    completed_count: UNITS,
                    request: None,

                    rolls: SavedItemRolls::default(),
                },
                None,
            )
            .and_then(|left| left.item)
            .map_or(0, |(_, waiting)| waiting);
        let held = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map_or(0, |gear| gear.bag_count(SINGLE));
        assert!(
            held > 0 && left > 0,
            "the bag filled part way: {held} {left}"
        );
        assert_eq!(held + left, UNITS);
    }

    /// The bag record sync reason of flint in the bag of `entity`.
    fn flint_reason(world: &SimWorld, entity: Entity) -> Option<u8> {
        let index = world
            .content
            .kits
            .item(gear::tests::FLINT)
            .expect("flint")
            .item_index;
        let player = world.world.get::<Player>(entity).expect("player");
        let Some(nw_messages::list_delta::ReplicatedList::Full(bag)) =
            player.gear.as_ref().expect("gear").states().bag.items
        else {
            panic!("a full bag list");
        };
        bag.elements
            .iter()
            .find(|record| record.item_index == index && record.quantity > 0)
            .map(|record| record.sync_reason)
    }

    /// Retail sync reasons: 19 on the stack a sell order takes, 10 on what a buyer receives, 0
    /// on what a cancel gives back; and a completer pays the tax of its own trading post.
    #[test]
    fn the_trading_post_moves_items_with_the_retail_sync_reasons_and_the_completers_tax() {
        const LISTED: u32 = 3;
        const STANDING_TAX: f64 = 0.0235;
        const PRICE: u64 = 10_000;
        let (mut world, seller, buyer) = traders(STANDING_TAX);
        world.contract_call(
            seller,
            ContractCall::Create {
                header: FacetHeader::of(CONTRACTS_COMPONENT, SELLER_NUMBER),
                order: CreateOrder {
                    kind: ContractKind::Sell.value(),
                    price: PRICE,
                    count: LISTED,
                    days: 3,
                    item_crc: lowercase_crc32(gear::tests::FLINT),
                    item_block: [0; nw_messages::contracts::CREATE_ITEM_BLOCK_LEN],
                },
                request: [1; RAW_ID_LEN],
                tail: 0,
            },
        );
        let jobs = world.take_economy_jobs();
        let Some(EconomyJob::Post { order, refund, .. }) = jobs.first() else {
            panic!("a post job: {jobs:?}");
        };
        assert_eq!(refund.items, Some((gear::tests::FLINT.to_owned(), LISTED)));
        assert_eq!(
            order.tax_rate, STANDING_TAX,
            "the order keeps its settlement's tax"
        );
        assert_eq!(flint_reason(&world, seller), Some(SYNC_TRADING_POST_TAKEN));
        let creator = world.world.get::<Player>(seller).expect("seller").character;
        let stored = Order {
            id: order.id,
            kind: OrderKind::Sell,
            status: OrderStatus::Available,
            version: 1,
            creator,
            outpost: OUTPOST,
            item: order.item.clone(),
            price: PRICE,
            posted: LISTED,
            open: LISTED,
            end: nw_db::OrderEnd::None,
            tax_rate: crate::trading_post::RETAIL_TRADING_TAX,
            created_ms: NOW,
            expires_ms: NOW + MS_PER_DAY,
            completed_ms: None,
            updated_ms: NOW,
        };
        let caller = world
            .caller(
                buyer,
                FacetHeader::of(CONTRACTS_COMPONENT, BUYER_NUMBER),
                [2; RAW_ID_LEN],
            )
            .expect("caller");
        world.complete_inspected(
            &caller,
            &stored,
            CompletionCall {
                kind: ContractKind::Sell.value(),
                units: 1,
                item_crc: 0,
            },
        );
        let paid = PRICE + nw_db::tax_of(PRICE, STANDING_TAX);
        assert_eq!(
            world.world.get::<Player>(buyer).expect("buyer").coins,
            COINS_HELD - paid,
            "the buyer pays its own trading post's tax, not the order's"
        );
        let jobs = world.take_economy_jobs();
        assert!(matches!(
            jobs.as_slice(),
            [EconomyJob::Complete { tax_rate, units: 1, .. }] if *tax_rate == STANDING_TAX
        ));
        let bought = Delivery {
            character: caller.character,
            order: Some(order.id),
            notice: DeliveryNotice::None,
            coins: 0,
            item: Some((gear::tests::FLINT.to_owned(), 1)),
            completed_count: 1,
            request: None,

            rolls: SavedItemRolls::default(),
        };
        assert_eq!(world.give(buyer, bought, None), None);
        assert_eq!(flint_reason(&world, buyer), Some(SYNC_TRADING_POST_BOUGHT));
        let returned = Refund {
            coins: 0,
            items: Some((gear::tests::FLINT.to_owned(), LISTED)),
            values: None,
        }
        .delivery(creator);
        assert_eq!(world.give(seller, returned, None), None);
        assert_eq!(
            flint_reason(&world, seller),
            Some(SYNC_TRADING_POST_RETURNED)
        );
        assert_eq!(
            world
                .world
                .get::<Player>(seller)
                .and_then(|player| player.gear.as_ref())
                .map(|gear| gear.bag_count(gear::tests::FLINT)),
            Some(STACK)
        );
    }
}

#[cfg(all(test, feature = "infra"))]
mod infra_tests {
    //! The trading post end to end on the Docker PostgreSQL world database: two players of one
    //! sim post, buy, sell and cancel through their calls, the jobs run on the database and
    //! their answers come back as the loop gives them.

    use super::*;
    use crate::trading_post::RETAIL_TRADING_TAX;
    use crate::world::gear::tests::FLINT;
    use crate::world::tests::messages_for;
    use nw_db::test_db::with_test_schema;
    use nw_db::{NewCharacter, OrderEnd, WORLD_MIGRATOR, insert_character, tax_of};
    use nw_messages::contracts::{CREATE_ITEM_BLOCK_LEN, Contract, status as contract_status};
    use nw_messages::item_descriptor::ShortItemDescriptor;

    use super::tests::{
        BUYER_NUMBER as BUYER, COINS_HELD as COINS, OUTPOST, SELLER_NUMBER as SELLER, STACK,
        traders,
    };

    const FIRST_BUY: u32 = 1;
    const PRICE: u64 = 10_000;
    const DAYS: u32 = 3;

    /// [`traders`], with their characters in the world database `db`.
    async fn stored_traders(db: &PgPool) -> (SimWorld, Entity, Entity) {
        let (world, seller, buyer) = traders(RETAIL_TRADING_TAX);
        for entity in [seller, buyer] {
            let player = world.world.get::<Player>(entity).expect("player");
            insert_character(
                db,
                &NewCharacter {
                    id: player.character,
                    account_id: Uuid::new_v4(),
                    name: format!("Trader{}", player.number),
                    appearance: serde_json::json!({}),
                },
            )
            .await
            .expect("insert");
        }
        (world, seller, buyer)
    }

    /// Runs the queued economy jobs and their follow-ups, then one delivery round, and returns
    /// what each player was sent.
    async fn settle(world: &mut SimWorld, db: &PgPool) -> [Vec<ContractMessage>; 2] {
        for round in 0..2 {
            if round == 1 {
                world.economy_round();
            }
            loop {
                let jobs = world.take_economy_jobs();
                if jobs.is_empty() {
                    break;
                }
                for job in jobs {
                    if let Some(result) = run_job(db, job).await {
                        world.economy_done(result);
                    }
                }
            }
        }
        let sent = std::mem::take(&mut world.combat_messages);
        [SELLER, BUYER].map(|number| {
            messages_for(&sent, &crate::world::tests::connection(number))
                .into_iter()
                .filter_map(|message| match message {
                    ServerMessage::Economy(EconomyMessage::Contract(contract)) => Some(*contract),
                    _ => None,
                })
                .collect()
        })
    }

    fn header(number: u64) -> FacetHeader {
        FacetHeader::of(CONTRACTS_COMPONENT, number)
    }

    fn coins(world: &SimWorld, entity: Entity) -> u64 {
        world.world.get::<Player>(entity).expect("player").coins
    }

    fn flint(world: &SimWorld, entity: Entity) -> u32 {
        world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map_or(0, |gear| gear.bag_count(FLINT))
    }

    fn post(world: &mut SimWorld, seller: Entity, count: u32) {
        post_as(world, seller, SELLER, ContractKind::Sell, count);
    }

    fn post_as(world: &mut SimWorld, entity: Entity, number: u64, kind: ContractKind, count: u32) {
        world.contract_call(
            entity,
            ContractCall::Create {
                header: header(number),
                order: CreateOrder {
                    kind: kind.value(),
                    price: PRICE,
                    count,
                    days: DAYS,
                    item_crc: lowercase_crc32(FLINT),
                    item_block: [0; CREATE_ITEM_BLOCK_LEN],
                },
                request: *Uuid::new_v4().as_bytes(),
                tail: 0,
            },
        );
    }

    fn complete(
        world: &mut SimWorld,
        entity: Entity,
        number: u64,
        order: Uuid,
        units: u32,
        how: u32,
    ) {
        complete_as(world, entity, number, order, ContractKind::Sell, units, how);
    }

    fn complete_as(
        world: &mut SimWorld,
        entity: Entity,
        number: u64,
        order: Uuid,
        kind: ContractKind,
        units: u32,
        how: u32,
    ) {
        world.contract_call(
            entity,
            ContractCall::Complete {
                header: header(number),
                contract: *order.as_bytes(),
                request: *Uuid::new_v4().as_bytes(),
                kind: kind.value(),
                count: units,
                reason: how,
                item: ShortItemDescriptor {
                    item_crc: lowercase_crc32(FLINT),
                    quantity: units,
                    gear_score: 0,
                    perks: Vec::new(),
                },
                tail: 0,
            },
        );
    }

    /// The contract of `order` in the search answer of `number` and in its own lists.
    async fn seen(
        world: &mut SimWorld,
        db: &PgPool,
        entity: Entity,
        number: u64,
        order: Uuid,
    ) -> (Option<Contract>, u32, Option<Contract>) {
        let caller = world
            .caller(entity, header(number), [0; RAW_ID_LEN])
            .expect("caller");
        world.economy.jobs.push(EconomyJob::Search {
            caller: caller.clone(),
            search: OrderSearch {
                kind: OrderKind::Sell,
                category: None,
                family: None,
                group: None,
                item_id: Some(FLINT.to_owned()),
                tier: None,
                outposts: vec![OUTPOST],
                perks: Vec::new(),
                start: 0,
                limit: MAX_PAGE,
                now_ms: unix_ms(),
            },
        });
        world.economy.jobs.push(EconomyJob::CharacterLookup {
            wanted: CharacterOrders {
                closed: true,
                completed: true,
                ..transfer_check_orders(caller.character)
            },
            caller,
        });
        let sent = settle(world, db).await;
        let index = usize::from(number == BUYER);
        let mut searched = None;
        let mut total = 0;
        let mut listed = None;
        for message in &sent[index] {
            match message {
                ContractMessage::SearchResults(results) => {
                    total = results.total;
                    searched = results
                        .contracts
                        .iter()
                        .find(|c| c.id == *order.as_bytes())
                        .cloned();
                }
                ContractMessage::LookedUp(list) => {
                    listed = list
                        .0
                        .contracts
                        .iter()
                        .find(|c| c.id == *order.as_bytes())
                        .cloned();
                }
                _ => {}
            }
        }
        (searched, total, listed)
    }

    /// The owner's case: A lists 10, B buys 1, both see 9; B buys the other 9, the listing is
    /// gone for both, B holds all 10 and A has the price of all 10 less the 5 % kept back
    /// (retail `buy-sell-TP` 224.298 s and `Hivesellandbuyorder` 262.326 s: 95 % of each sale).
    #[tokio::test]
    async fn a_stack_of_ten_bought_one_then_nine_shows_the_rest_to_both_and_pays_out() {
        let checked = with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let (mut world, seller, buyer) = stored_traders(db).await;
            post(&mut world, seller, STACK);
            let sent = settle(&mut world, db).await;
            let created = sent[0]
                .iter()
                .find_map(|message| match message {
                    ContractMessage::Notice(notice)
                        if notice.type_index == CONTRACT_CREATED_TYPE_INDEX =>
                    {
                        Some(notice.contract.clone())
                    }
                    _ => None,
                })
                .expect("634");
            assert_eq!(created.version, 0, "retail 634 v0");
            assert_eq!(created.quantity, STACK);
            assert_eq!(flint(&world, seller), 0, "the stack is listed");
            let order = Uuid::from_bytes(created.id);

            complete(
                &mut world,
                buyer,
                BUYER,
                order,
                FIRST_BUY,
                reason::SUCCEEDED,
            );
            let sent = settle(&mut world, db).await;
            let resolved = sent[1]
                .iter()
                .find_map(|message| match message {
                    ContractMessage::Notice(notice)
                        if notice.type_index == CONTRACT_RESOLVED_TYPE_INDEX =>
                    {
                        Some(notice.contract.clone())
                    }
                    _ => None,
                })
                .expect("1117");
            let left = STACK - FIRST_BUY;
            assert_eq!(resolved.status, contract_status::AVAILABLE);
            assert_eq!(resolved.quantity, left);
            assert!(
                sent[0].is_empty(),
                "no retail notice of a part sale to its seller"
            );
            assert_eq!(flint(&world, buyer), FIRST_BUY);
            let tax = tax_of(PRICE, RETAIL_TRADING_TAX);
            assert_eq!(coins(&world, buyer), COINS - PRICE - tax);
            assert_eq!(coins(&world, seller), COINS + PRICE - tax);
            for (entity, number) in [(seller, SELLER), (buyer, BUYER)] {
                let (searched, total, listed) = seen(&mut world, db, entity, number, order).await;
                let searched = searched.expect("found");
                assert_eq!(total, 1);
                assert_eq!(searched.quantity, left);
                assert_eq!(searched.events[0].1.completion_count, left);
                assert_eq!(searched.sold_item.map(|item| item.quantity), Some(left));
                assert_eq!(listed.expect("listed").quantity, left);
            }

            complete(&mut world, buyer, BUYER, order, left, reason::SUCCEEDED);
            let sent = settle(&mut world, db).await;
            let fully = sent[0]
                .iter()
                .find_map(|message| match message {
                    ContractMessage::FullyCompleted(fully) => Some(fully.clone()),
                    _ => None,
                })
                .expect("2964");
            assert_eq!(fully.contract.status, contract_status::COMPLETED);
            assert_eq!(fully.contract.completed_ns, 0);
            assert!(sent[0].iter().any(|message| matches!(
                message,
                ContractMessage::Notice(notice)
                    if notice.type_index == CONTRACT_RESOLVED_TYPE_INDEX
                        && notice.contract.status == contract_status::RESOLVED
            )));
            assert_eq!(flint(&world, buyer), STACK, "the buyer holds all 10");
            let rest = u64::from(left) * PRICE;
            let rest_tax = tax_of(rest, RETAIL_TRADING_TAX);
            assert_eq!(coins(&world, buyer), COINS - PRICE - tax - rest - rest_tax);
            assert_eq!(coins(&world, seller), COINS + PRICE - tax + rest - rest_tax);
            for (entity, number) in [(seller, SELLER), (buyer, BUYER)] {
                let (searched, total, listed) = seen(&mut world, db, entity, number, order).await;
                assert!(searched.is_none() && total == 0, "the listing is gone");
                let listed = listed.expect("still in the lists");
                assert_eq!(listed.quantity, 0);
                assert_eq!(listed.status, contract_status::RESOLVED);
                assert_eq!(listed.events[0].1.reason, OrderEnd::Succeeded.value());
                assert_eq!(listed.events[0].1.completion_count, STACK);
            }
            true
        })
        .await;
        assert!(checked);
    }

    /// A cancel of a partly bought listing gives the units left back to the seller's bag;
    /// the buyer keeps what it bought, and another buy of it is refused.
    #[tokio::test]
    async fn a_cancel_after_a_part_sale_gives_the_rest_back() {
        const BOUGHT: u32 = 4;
        let checked = with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let (mut world, seller, buyer) = stored_traders(db).await;
            post(&mut world, seller, STACK);
            let sent = settle(&mut world, db).await;
            let order = sent[0]
                .iter()
                .find_map(|message| match message {
                    ContractMessage::Notice(notice) => Some(Uuid::from_bytes(notice.contract.id)),
                    _ => None,
                })
                .expect("634");
            complete(&mut world, buyer, BUYER, order, BOUGHT, reason::SUCCEEDED);
            settle(&mut world, db).await;
            complete(&mut world, seller, SELLER, order, 0, reason::CANCELED);
            let sent = settle(&mut world, db).await;
            let canceled = sent[0]
                .iter()
                .find_map(|message| match message {
                    ContractMessage::Notice(notice)
                        if notice.type_index == CONTRACT_CANCELED_TYPE_INDEX =>
                    {
                        Some(notice.contract.clone())
                    }
                    _ => None,
                })
                .expect("2965");
            assert_eq!(canceled.status, contract_status::RESOLVED);
            assert_eq!(canceled.quantity, STACK - BOUGHT);
            assert_eq!(canceled.events[0].1.reason, reason::CANCELED);
            assert_eq!(flint(&world, seller), STACK - BOUGHT);
            assert_eq!(flint(&world, buyer), BOUGHT);
            complete(&mut world, buyer, BUYER, order, 1, reason::SUCCEEDED);
            let sent = settle(&mut world, db).await;
            assert!(sent[1].iter().any(|message| matches!(
                message,
                ContractMessage::Failed(failed) if failed.reason == failure::ALREADY_RESOLVED
            )));
            assert_eq!(flint(&world, buyer), BOUGHT);
            true
        })
        .await;
        assert!(checked);
    }

    /// A buy order of 10 filled by 4 then 6: the seller's flint leaves its bag and it gets the
    /// price less its tax each time; the creator's 10 go to the storage of the order's
    /// settlement, with no bag change (retail `buy-sell-TP` 328.900 s, `Hivesellandbuyorder`
    /// 401.71 s).
    #[tokio::test]
    async fn a_buy_order_filled_in_parts_fills_the_settlements_storage() {
        const FIRST_SALE: u32 = 4;
        let checked = with_test_schema(async |db| {
            WORLD_MIGRATOR.run(db).await.expect("migrate");
            let (mut world, seller, buyer) = stored_traders(db).await;
            post_as(&mut world, buyer, BUYER, ContractKind::Buy, STACK);
            let sent = settle(&mut world, db).await;
            let order = sent[1]
                .iter()
                .find_map(|message| match message {
                    ContractMessage::Notice(notice) => Some(Uuid::from_bytes(notice.contract.id)),
                    _ => None,
                })
                .expect("634");
            let escrow = u64::from(STACK) * PRICE;
            assert_eq!(coins(&world, buyer), COINS - escrow);
            for units in [FIRST_SALE, STACK - FIRST_SALE] {
                complete_as(
                    &mut world,
                    seller,
                    SELLER,
                    order,
                    ContractKind::Buy,
                    units,
                    reason::SUCCEEDED,
                );
                settle(&mut world, db).await;
            }
            let first = u64::from(FIRST_SALE) * PRICE;
            let rest = escrow - first;
            assert_eq!(
                coins(&world, seller),
                COINS + first - tax_of(first, RETAIL_TRADING_TAX) + rest
                    - tax_of(rest, RETAIL_TRADING_TAX)
            );
            assert_eq!(flint(&world, seller), 0);
            assert_eq!(flint(&world, buyer), 0, "no bag change");
            let stored = world
                .world
                .get::<Player>(buyer)
                .and_then(|player| player.gear.as_ref())
                .and_then(|gear| gear.stored_totals().get(FLINT).copied());
            assert_eq!(stored, Some(STACK));
            true
        })
        .await;
        assert!(checked);
    }
}
