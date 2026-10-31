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
use nw_bus::ConnectionRef;
use nw_common::time::unix_ms;
use nw_content::progression::Mastery;
use nw_db::{
    CharacterOrders, Completed, DbError, Delivery, DeliveryNotice, NewOrder, Order, OrderKind,
    OrderPage, OrderRefusal, OrderSearch, OrderStatus, PgPool, TakenDelivery, TradeSide,
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
use nw_messages::items::{FullList, ItemRecord, SYNC_OWNED};
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

use super::gear::{Gear, MAX_CONTAINER_SLOTS};
use super::quest_objects::with_coins;
use super::{Player, SimWorld, gather_answer, send_now};
use crate::shop::ShopOffer;
use crate::trading_post::{
    MS_PER_DAY, TradeItem, completion_cost, contract_of, next_page_token, order_kind,
};

/// The interaction kind of a trading post (`InteractionUIActions` `OpenTradingPost`).
pub(super) const TRADING_POST_KIND: u8 = 21;
/// The status of an accepted interaction.
const INTERACTION_ACCEPTED: u8 = 1;
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
        }
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
        order: NewOrder,
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
            refund,
        } => EconomyResult::Completed {
            completed: nw_db::complete_order(
                world,
                order,
                caller.character,
                units,
                Uuid::from_bytes(caller.request),
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
        contract: contract_of(order, caller.character),
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

fn count_call(call: &'static str, accepted: bool) {
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
    /// ([`TRADING_POST_ANSWER_EXTRA`]); the post's settlement is the territory the player is in.
    pub(super) fn open_trading_post(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
        name: &[u8],
    ) {
        let Some(player) = self.world.get::<Player>(entity) else {
            return;
        };
        let outpost = player
            .territories
            .first()
            .map_or(0, |territory| u64::from(*territory));
        let connection = player.connection.clone();
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
        let gear_score = match kind {
            OrderKind::Sell => content
                .kits
                .item(&item.item_id)
                .map_or(0, |kit| kit.gear_score),
            OrderKind::Buy => 0,
        };
        let refund = match self.take_for_order(entity, kind, &item, order.count, total, fee) {
            Ok(refund) => refund,
            Err(failure_reason) => return refused(self, failure_reason),
        };
        count_call("create", true);
        self.economy.jobs.push(EconomyJob::Post {
            order: NewOrder {
                id: Uuid::new_v4(),
                kind,
                creator: caller.character,
                outpost,
                item: item.order_item(gear_score),
                price: order.price,
                quantity: order.count,
                tax_rate,
                created_ms: now_ms,
                expires_ms: now_ms.saturating_add(u64::from(order.days) * MS_PER_DAY),
                posting_fee: fee,
                max_open,
            },
            caller,
            refund,
        });
    }

    /// Takes from player `entity` what a new order of `count` units of `item` worth `total`
    /// costs: the posting fee, and a buy order's total or a sell order's units.
    fn take_for_order(
        &mut self,
        entity: Entity,
        kind: OrderKind,
        item: &TradeItem,
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
        let items = match kind {
            OrderKind::Sell => {
                let taken = player
                    .gear
                    .as_mut()
                    .is_some_and(|gear| gear.take(&item.item_id, count));
                if !taken {
                    return Err(failure::INSUFFICIENT_ITEMS);
                }
                Some((item.item_id.clone(), count))
            }
            OrderKind::Buy => None,
        };
        if coins > 0 {
            player.coins -= coins;
            player.coins_changed = true;
        }
        Ok(Refund { coins, items })
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
                |entity| self.give(entity, refund.delivery(caller.character)),
            );
        if let Some(leftover) = leftover {
            self.economy.jobs.push(EconomyJob::Return {
                deliveries: vec![leftover],
            });
        }
    }

    /// Gives `delivery`'s coins (under the wallet cap) and items to player `entity`; what does
    /// not fit comes back.
    fn give(&mut self, entity: Entity, delivery: Delivery) -> Option<Delivery> {
        let wallet_cap = self.content.gameplay.wallet_cap;
        let kit = delivery
            .item
            .as_ref()
            .and_then(|(item, _)| self.content.kits.item(item).cloned());
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
            let given = match (&kit, player.gear.as_mut()) {
                (Some(kit), Some(gear)) => gear.add(kit, quantity),
                _ => false,
            };
            if !given {
                tracing::warn!(
                    character = %delivery.character,
                    item = %item,
                    quantity,
                    "a delivery's items do not fit; they wait"
                );
                leftover.item = Some((item, quantity));
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
                            .map(|order| contract_of(order, caller.character))
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
                            .map(|(order, _)| contract_of(order, caller.character))
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
                .map(|order| contract_of(order, caller.character))
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
        let Some((coins, items)) = completion_cost(order, call.units) else {
            return self.reply(caller, &[action_failed(caller, failure::INVALID_REQUEST)]);
        };
        let Some(entity) = self.by_connection.get(&caller.connection).copied() else {
            return;
        };
        let taken = {
            let Some(mut player) = self.world.get_mut::<Player>(entity) else {
                return;
            };
            let paid = if player.coins < coins {
                Err(failure::INSUFFICIENT_GOLD)
            } else if let Some((item, units)) = &items {
                let taken = player
                    .gear
                    .as_mut()
                    .is_some_and(|gear| gear.take(item, *units));
                if taken {
                    Ok(())
                } else {
                    Err(failure::INSUFFICIENT_ITEMS)
                }
            } else {
                Ok(())
            };
            if paid.is_ok() && coins > 0 {
                player.coins -= coins;
                player.coins_changed = true;
            }
            paid
        };
        if let Err(failure_reason) = taken {
            return self.reply(caller, &[action_failed(caller, failure_reason)]);
        }
        self.economy.jobs.push(EconomyJob::Complete {
            caller: caller.clone(),
            order: order.id,
            units: call.units,
            refund: Refund { coins, items },
        });
    }

    /// Gives the taken deliveries to their players with their notices (2964 then 1117 for a
    /// fully completed order, 917 for an expired one); what is not given is owed again.
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
                    let contract = contract_of(order, character);
                    vec![
                        contract_message(ContractMessage::FullyCompleted(FullyCompletedMsg {
                            header,
                            request,
                            contract: contract.clone(),
                            count: delivery.completed_count,
                        })),
                        contract_message(ContractMessage::Notice(ContractNoticeMsg {
                            type_index: CONTRACT_RESOLVED_TYPE_INDEX,
                            header,
                            request,
                            contract,
                        })),
                    ]
                }
                (Some(order), DeliveryNotice::Expired) => {
                    vec![contract_message(ContractMessage::Notice(
                        ContractNoticeMsg {
                            type_index: CONTRACT_EXPIRED_TYPE_INDEX,
                            header,
                            request: expiry_batch,
                            contract: contract_of(order, character),
                        },
                    ))]
                }
                _ => Vec::new(),
            };
            if let Some(leftover) = self.give(entity, delivery) {
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
            StoreCall::Purchase { purchase, .. } => {
                // This server sells nothing: no store purchase and no paid currency.
                count_call("store_purchase", false);
                tracing::info!(
                    product = %String::from_utf8_lossy(&purchase.product),
                    "store purchase refused: this server has no store"
                );
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

    /// Moves `a`'s offer to `b` and `b`'s to `a`; the audit sides, or why nothing moved.
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
        let kits: Vec<_> = a_items
            .iter()
            .chain(&b_items)
            .map(|(_, item, _)| self.content.kits.item(item).cloned())
            .collect();
        if kits.iter().any(Option::is_none) {
            return Err(trade_failure::INVALID_ITEMS);
        }
        for (side, items) in [(a, &a_items), (b, &b_items)] {
            let mut player = self
                .world
                .get_mut::<Player>(side.entity)
                .ok_or(trade_failure::FAILED_TO_REMOVE)?;
            if !items.is_empty() {
                let Some(gear) = player.gear.as_mut() else {
                    return Err(trade_failure::FAILED_TO_REMOVE);
                };
                for (slot, _, quantity) in items.iter() {
                    if !gear.take_from_slot(*slot, *quantity) {
                        return Err(trade_failure::FAILED_TO_REMOVE);
                    }
                }
            }
            player.coins -= side.coin;
            player.coins_changed = true;
        }
        let mut kits = kits.into_iter().flatten();
        for (side, items, coin) in [(b, &a_items, a.coin), (a, &b_items, b.coin)] {
            let mut player = self
                .world
                .get_mut::<Player>(side.entity)
                .ok_or(trade_failure::CANNOT_RECEIVE_ITEMS)?;
            player.coins = player.coins.saturating_add(coin);
            player.coins_changed = true;
            for (_, item, quantity) in items {
                let Some(kit) = kits.next() else {
                    return Err(trade_failure::CANNOT_RECEIVE_ITEMS);
                };
                let added = player
                    .gear
                    .as_mut()
                    .is_some_and(|gear| gear.add(&kit, *quantity));
                if !added {
                    tracing::error!(item = %item, quantity, "a traded stack did not fit the bag");
                }
            }
        }
        let audit = |side: &TradeSideState, items: &[(u32, String, u32)]| TradeSide {
            character: side.character,
            coins: side.coin,
            items: items
                .iter()
                .map(|(_, item, quantity)| (item.clone(), *quantity))
                .collect(),
        };
        Ok((audit(a, &a_items), audit(b, &b_items)))
    }

    /// The bag slot, item id and quantity of each stack `side` offers, still in its bag.
    fn offered_items(&self, side: &TradeSideState) -> Result<Vec<(u32, String, u32)>, u32> {
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
                            .then(|| (slot, item.to_owned(), quantity))
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
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, world_with_spawns};
    use nw_content::generated::ItemCurrencyConversionData;
    use nw_content::progression::{Progression, ProgressionSource};
    use nw_messages::contracts::Participant;

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
}
