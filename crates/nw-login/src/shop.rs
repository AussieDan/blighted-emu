// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shop and entitlement services (study note `docs/re/2026-11-02-shop-services.md`). The
//! client's `FcsEndpointClient` (`FUN_1416190f0`) calls them at the hosts of the remote config
//! keys `omni.fcs.endpoints.*`, which this server fills with `game.<domain>`
//! ([`crate::remote_config::GAME_HOST_ENDPOINTS`]). Without the keys the client calls Amazon.
//!
//! While `[shop] enabled` is on, the catalog offers every mount of the store table
//! ([`nw_protocol::store_api`]): for 0 Marks of Fortune while `[shop] free` is on (retail's last
//! weeks, `mount-menu-buying-all-store-horses`, 2026-11-04), else for its price. The entitlement
//! list holds what every account holds (the expansion), the account's wallet and what the account
//! got (global database). A purchase goes to the sim over the game connection (898), not to these
//! routes. An offer's alias is its mount's store product, every store is hidden with one empty
//! slot, and a sync is accepted. The shapes, the `ETag` and the `304` follow the retail replies
//! (`Loadouts` capture: 368 offers, 69 of them mounts).
//!
//! The persona of a path is the player's account id (`PersonaId` of the login). Inferred: the
//! client sends no proof of its account to these routes that this server checks, so the
//! entitlement list of an account answers whoever names its id; it holds only store mounts.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::header::{CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use data_encoding::BASE64;
use nw_common::time::iso8601_millis;
use nw_db::{AccountEntitlement, Wallet};
use nw_protocol::store_api::{STORE_CURRENCY, STORE_CURRENCY_NAME, StoreEntitlement, StoreTable};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::ShopConfig;
use crate::error::ApiError;
use crate::store::EntitlementStore;

/// The route prefix: the player's persona, the game alias and the platform.
const PLAYER_GAME_PREFIX: &str = "/players/{persona}/games/new-world/platforms/{platform}";
/// Currency and country of the prices. Inferred: retail answers in the player's own currency
/// and country; this server has one region, `us-east`.
const CURRENCY_CODE: &str = "USD";
const COUNTRY_CODE: &str = "US";
/// Content type of a reply (retail).
const JSON_CONTENT_TYPE: &str = "application/json";
/// Content type of a `304` (retail).
const NOT_MODIFIED_CONTENT_TYPE: &str = "text/plain; charset=utf-8";
/// The header the client names its cached ETag in (live 2026-11-04: every catalog and store call
/// after the first carried it, never `If-None-Match`). Retail answered such calls `304`
/// (`unlock_store_content` capture: every store call after the first).
const CLIENT_IF_NONE_MATCH: HeaderName = HeaderName::from_static("x-amzn-if-none-match");
/// Between the parts of a retail persona (`amzn1.developerPersonaId.<id>`).
const PERSONA_SEPARATOR: char = '.';
/// The price kind of the store currency (retail `virtualCurrencyPrices[].type`).
const CURRENCY_KIND: &str = "HardVirtualCurrency";
/// The offer kind and product kind of a retail mount offer.
const OFFER_TYPE: &str = "MTx";
const PRODUCT_TYPE: &str = "PRODUCT";
/// The metadata of a retail mount offer.
const MOUNT_OFFER_METADATA: &str = r#"{"type":"Mount"}"#;
/// The entitlement kind of a mount (retail `Durable`), of the store currency (`Wallet`) and of
/// the transmog tokens (`Consumable`).
const DURABLE: &str = "Durable";
const WALLET: &str = "Wallet";
const CONSUMABLE: &str = "Consumable";
/// The transmog token entitlement (retail `unlock_store_content`: alias `Transmog_Token`, name
/// `Transmog Token`). The inventory's token counter (`transmogtokenscontainer.luac`) shows a
/// spinner until the list holds it (`GetEntitlementBalance` of the CRC of `transmog_token`).
const TRANSMOG_TOKEN: &str = "Transmog_Token";
const TRANSMOG_TOKEN_NAME: &str = "Transmog Token";
/// The transmog tokens of every account. Inferred: this server grants none and charges none (a
/// skin of the collection is applied for free, `bins/sim/src/world/transmog.rs`).
const TRANSMOG_TOKENS_HELD: i64 = 0;
/// The description of a retail mount offer: its name and this.
const DURABLE_SUFFIX: &str = " Durable";
/// When every offer and every entitlement held by all accounts starts: the expansion's store
/// mounts (retail `productStartDate` of `Mount_Wolf_1`).
const STORE_START: &str = "2023-10-03T13:30:00.000Z";
/// How many of an offer an account may get, and the amount of a held entitlement.
const ONE: u32 = 1;
/// The discount retail shows on a free offer (10386 of 10386 free retail prices: 99, not 100).
const FREE_DISCOUNT: u32 = 99;

/// The shop's state: the store table, the `[shop]` settings and the accounts' stores.
#[derive(Clone)]
pub struct ShopState<E> {
    /// What the store sells and what every account holds.
    pub table: Arc<StoreTable>,
    /// Whether the store sells, and its prices.
    pub settings: Arc<ShopConfig>,
    /// The accounts' entitlements and wallets.
    pub entitlements: E,
}

/// `GET .../catalog?type=MTX`: the offers for sale.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Catalog<'a> {
    currency_code: &'static str,
    offers: &'a [Offer],
    country_code: &'static str,
    etag: String,
}

/// One offer (retail shape of a mount offer).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Offer {
    is_refundable: bool,
    acquisition_count: u32,
    entitlements: [OfferEntitlement; 1],
    items: [(); 0],
    end_date: Option<&'static str>,
    start_date: &'static str,
    in_game_currency_prices: [InGamePrice; 1],
    virtual_currency_prices: [VirtualPrice; 1],
    offer_type: &'static str,
    product_start_date: &'static str,
    product_end_date: Option<&'static str>,
    metadata: &'static str,
    name: String,
    #[serde(rename = "type")]
    kind: &'static str,
    description: String,
    alias: String,
    product_id: String,
    offer_id: String,
    max_acquisition_count: u32,
    required_items: Option<()>,
    external_offer_id: Option<()>,
    price: Option<()>,
}

/// The entitlement an offer gives.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OfferEntitlement {
    metadata: Option<()>,
    duration: Option<()>,
    name: String,
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    description: Option<()>,
    alias: String,
    amount: u32,
    external_items: [(); 0],
}

/// An offer's price in the store currency.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InGamePrice {
    sales_price_with_discount: u32,
    discount_percentage: u32,
    original_price: u32,
    currency_entitlement_alias: &'static str,
    currency_entitlement_id: &'static str,
    currency_entitlement_name: &'static str,
}

/// The same price as a virtual currency price.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VirtualPrice {
    currency_name: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    sales_price_with_discount: u32,
    discount_percentage: u32,
    original_price: u32,
    currency_id: &'static str,
    currency_alias: &'static str,
}

/// `GET .../stores/<alias>`: the stores with this alias. Retail always sends one, with that
/// alias (`leaving_a_company` capture: `Featured`, `Seasonal`, `Special1`, `Special2`). An empty
/// list left the client's catalog unfinished: it sent no `store_on_catalog_ready` and the change
/// mount screen, which waits for that (`selectionpanel.luac` `SetupMountsList`), spun (live
/// 2026-11-03).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Stores {
    currency_code: &'static str,
    stores: [Store; 1],
    country_code: &'static str,
    etag: String,
}

/// One store: hidden, with one empty slot. Retail's hidden stores have the metadata
/// `{"visible":"false"}` (`Special1`), and every one of the 1,029 retail store replies had one slot
/// or more (`Featured`: 6 with no offer). A store with no slot left the change mount screen
/// spinning (live 2026-11-04).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Store {
    metadata: &'static str,
    slots: [Slot; 1],
    next_schedule_start_date: Option<&'static str>,
    alias: String,
}

/// The metadata of a hidden store (retail `Special1`).
const HIDDEN_STORE_METADATA: &str = r#"{"visible":"false"}"#;

/// A store slot: static, with no offer (retail `Featured`; a hidden store's slot metadata is the
/// text `null`, `Special1`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Slot {
    metadata: &'static str,
    offers: [(); 0],
    slot_type: &'static str,
}

/// The metadata of a hidden store's slot (retail `Special1`).
const HIDDEN_SLOT_METADATA: &str = "null";
/// The kind of a slot whose offers do not rotate (retail).
const STATIC_SLOT: &str = "Static";

/// `GET .../entitlements`: one page of the player's entitlements. This server sends one page: the
/// last, whose `paginationToken` is `null` (retail `unlock_store_content`: 7 pages with a token,
/// then the last with `null`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entitlements {
    has_more_results: bool,
    entitlements: Vec<HeldEntitlement>,
    pagination_token: Option<String>,
}

/// One held entitlement (retail shape).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeldEntitlement {
    name: String,
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    alias: String,
    amount: HeldAmount,
    last_updated_date: String,
    external_items: [(); 0],
}

/// The amount of a held entitlement: its instances, or the balance of a wallet or a consumable
/// (retail `freeAmount`, `paidAmount`, `pendingAmount`, `lossAmount`, `totalAmount`).
#[derive(Serialize)]
#[serde(untagged)]
enum HeldAmount {
    /// A durable entitlement.
    #[serde(rename_all = "camelCase")]
    Instances {
        instance_ids: [Instance; 1],
        total_amount: u32,
    },
    /// A balance. Everything of this server is free (nothing is bought with money).
    #[serde(rename_all = "camelCase")]
    Balance {
        free_amount: i64,
        paid_amount: i64,
        pending_amount: i64,
        loss_amount: i64,
        total_amount: i64,
    },
}

/// One instance: its id and empty metadata.
#[derive(Serialize)]
struct Instance {
    id: String,
    metadata: serde_json::Map<String, serde_json::Value>,
}

impl HeldEntitlement {
    /// `entitlement`, named as the store table names it, with one instance `instance` that
    /// changed at `updated`.
    fn of(entitlement: &StoreEntitlement, instance: Uuid, updated: String) -> Self {
        Self {
            name: entitlement.name.clone(),
            id: entitlement.entitlement.clone(),
            kind: DURABLE,
            alias: entitlement.entitlement.clone(),
            amount: HeldAmount::Instances {
                instance_ids: [Instance {
                    id: instance.to_string(),
                    metadata: serde_json::Map::new(),
                }],
                total_amount: ONE,
            },
            last_updated_date: updated,
            external_items: [],
        }
    }

    /// The store currency entry of `wallet`.
    fn wallet(wallet: Wallet) -> Self {
        Self::balance(
            STORE_CURRENCY,
            STORE_CURRENCY_NAME,
            WALLET,
            wallet.marks,
            if wallet.updated_at_ms > 0 {
                iso8601_millis(u64::try_from(wallet.updated_at_ms).unwrap_or_default())
            } else {
                STORE_START.to_owned()
            },
        )
    }

    /// The transmog token entry.
    fn transmog_tokens() -> Self {
        Self::balance(
            TRANSMOG_TOKEN,
            TRANSMOG_TOKEN_NAME,
            CONSUMABLE,
            TRANSMOG_TOKENS_HELD,
            STORE_START.to_owned(),
        )
    }

    /// The `kind` entry `alias`, named `name`, with `amount` free units, changed at `updated`.
    fn balance(
        alias: &str,
        name: &str,
        kind: &'static str,
        amount: i64,
        updated: String,
    ) -> Self {
        Self {
            name: name.to_owned(),
            id: alias.to_owned(),
            kind,
            alias: alias.to_owned(),
            amount: HeldAmount::Balance {
                free_amount: amount,
                paid_amount: 0,
                pending_amount: 0,
                loss_amount: 0,
                total_amount: amount,
            },
            last_updated_date: updated,
            external_items: [],
        }
    }
}

/// The path of a player's call.
#[derive(Deserialize)]
struct PlayerPath {
    persona: String,
}

/// The account a persona names: the id itself, or the last part of a retail-style persona.
pub(crate) fn account_of(persona: &str) -> Option<Uuid> {
    persona
        .rsplit(PERSONA_SEPARATOR)
        .next()
        .and_then(|id| Uuid::parse_str(id).ok())
}

/// The entitlements the account of `persona` got; none for a persona that names no account.
async fn held_by<E: EntitlementStore>(
    shop: &ShopState<E>,
    persona: &str,
) -> Result<Vec<AccountEntitlement>, ApiError> {
    match account_of(persona) {
        Some(account) => shop
            .entitlements
            .held(account)
            .await
            .map_err(|_| ApiError::Unavailable),
        None => Ok(Vec::new()),
    }
}

/// The wallet of the account of `persona`; an empty one for a persona that names no account.
async fn wallet_of<E: EntitlementStore>(
    shop: &ShopState<E>,
    persona: &str,
) -> Result<Wallet, ApiError> {
    match account_of(persona) {
        Some(account) => shop
            .entitlements
            .wallet(account)
            .await
            .map_err(|_| ApiError::Unavailable),
        None => Ok(Wallet::default()),
    }
}

/// The prices of an offer: what it costs and its listed price, in Marks of Fortune.
#[derive(Clone, Copy)]
struct Price {
    cost: u32,
    listed: u32,
}

impl Price {
    /// The discount the store shows: retail's for a free offer.
    fn discount(self) -> u32 {
        if self.cost == 0 && self.listed > 0 {
            FREE_DISCOUNT
        } else {
            0
        }
    }
}

/// The offer of the store mount entitlement `unlock` for the store product `product`, at
/// `price`, got `count` times.
fn offer(unlock: &StoreEntitlement, product: &str, price: Price, count: u32) -> Offer {
    Offer {
        is_refundable: false,
        acquisition_count: count,
        entitlements: [OfferEntitlement {
            metadata: None,
            duration: None,
            name: unlock.name.clone(),
            id: unlock.entitlement.clone(),
            kind: DURABLE,
            description: None,
            alias: unlock.entitlement.clone(),
            amount: ONE,
            external_items: [],
        }],
        items: [],
        end_date: None,
        start_date: STORE_START,
        in_game_currency_prices: [InGamePrice {
            sales_price_with_discount: price.cost,
            discount_percentage: price.discount(),
            original_price: price.listed,
            currency_entitlement_alias: STORE_CURRENCY,
            currency_entitlement_id: STORE_CURRENCY,
            currency_entitlement_name: STORE_CURRENCY_NAME,
        }],
        virtual_currency_prices: [VirtualPrice {
            currency_name: STORE_CURRENCY_NAME,
            kind: CURRENCY_KIND,
            sales_price_with_discount: price.cost,
            discount_percentage: price.discount(),
            original_price: price.listed,
            currency_id: STORE_CURRENCY,
            currency_alias: STORE_CURRENCY,
        }],
        offer_type: OFFER_TYPE,
        product_start_date: STORE_START,
        product_end_date: None,
        metadata: MOUNT_OFFER_METADATA,
        name: unlock.name.clone(),
        kind: PRODUCT_TYPE,
        description: format!("{}{DURABLE_SUFFIX}", unlock.name),
        alias: product.to_owned(),
        product_id: unlock.entitlement.clone(),
        offer_id: unlock.entitlement.clone(),
        max_acquisition_count: ONE,
        required_items: None,
        external_offer_id: None,
        price: None,
    }
}

/// The ETag of a reply: base64 of the SHA-256 of its body without the ETag. Inferred from the
/// retail form (44 base64 characters).
fn etag_of(body: &[u8]) -> String {
    BASE64.encode(&Sha256::digest(body))
}

/// The shop routes, merged into the routes of `game.<domain>`.
pub fn shop_router<E: EntitlementStore>(shop: ShopState<E>) -> Router {
    Router::new()
        .route(&format!("{PLAYER_GAME_PREFIX}/catalog"), get(catalog::<E>))
        .route(
            &format!("{PLAYER_GAME_PREFIX}/stores/{{alias}}"),
            get(stores),
        )
        .route(
            &format!("{PLAYER_GAME_PREFIX}/entitlements"),
            get(entitlements::<E>),
        )
        .route(
            &format!("{PLAYER_GAME_PREFIX}/entitlements/sync"),
            post(entitlements_sync),
        )
        .layer(middleware::from_fn(log_store_call))
        .with_state(shop)
}

/// One shop call as the log shows it: method, path, query, the names of its headers (never their
/// values) and the status.
struct StoreCallLine<'a> {
    method: &'a str,
    path: &'a str,
    query: &'a str,
    headers: Vec<&'a str>,
    status: u16,
}

impl std::fmt::Display for StoreCallLine<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}?{} headers={} status={}",
            self.method,
            self.path,
            self.query,
            self.headers.join(","),
            self.status
        )
    }
}

/// Logs each shop call, so a live run shows what the client asks the shop for (the change mount
/// screen waits on the catalog) and which headers could tell who asks.
async fn log_store_call(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let names: Vec<HeaderName> = request.headers().keys().cloned().collect();
    let response = next.run(request).await;
    let line = StoreCallLine {
        method: method.as_str(),
        path: uri.path(),
        query: uri.query().unwrap_or_default(),
        headers: names.iter().map(HeaderName::as_str).collect(),
        status: response.status().as_u16(),
    };
    tracing::info!(call = %line, "store call");
    response
}

/// A reply with an ETag in its body and its header, or `304` when the client's
/// [`CLIENT_IF_NONE_MATCH`] or `If-None-Match` names it.
/// `with_etag` builds the reply with the ETag it is given; the ETag is taken over the reply with
/// an empty one.
fn tagged_reply<T: Serialize>(headers: &HeaderMap, with_etag: impl Fn(String) -> T) -> Response {
    let Ok(untagged) = serde_json::to_vec(&with_etag(String::new())) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let etag = etag_of(&untagged);
    let Ok(etag_header) = HeaderValue::from_str(&etag) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let known = [CLIENT_IF_NONE_MATCH, IF_NONE_MATCH].iter().any(|name| {
        headers
            .get(name)
            .is_some_and(|value| value.as_bytes() == etag.as_bytes())
    });
    if known {
        return (
            StatusCode::NOT_MODIFIED,
            [(
                CONTENT_TYPE,
                HeaderValue::from_static(NOT_MODIFIED_CONTENT_TYPE),
            )],
        )
            .into_response();
    }
    let Ok(body) = serde_json::to_vec(&with_etag(etag)) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (
        [
            (CONTENT_TYPE, HeaderValue::from_static(JSON_CONTENT_TYPE)),
            (ETAG, etag_header),
        ],
        body,
    )
        .into_response()
}

async fn catalog<E: EntitlementStore>(
    State(shop): State<ShopState<E>>,
    Path(path): Path<PlayerPath>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let held = held_by(&shop, &path.persona).await?;
    let sold = if shop.settings.enabled {
        shop.table.mounts.as_slice()
    } else {
        &[]
    };
    let offers: Vec<Offer> = sold
        .iter()
        .map(|mount| {
            let entitlement = &mount.unlock.entitlement;
            let got = held.iter().any(|held| held.entitlement == *entitlement);
            let price = Price {
                cost: shop.settings.price(entitlement),
                listed: shop.settings.list_price(entitlement),
            };
            offer(&mount.unlock, &mount.product, price, u32::from(got))
        })
        .collect();
    Ok(tagged_reply(&headers, |etag| Catalog {
        currency_code: CURRENCY_CODE,
        offers: &offers,
        country_code: COUNTRY_CODE,
        etag,
    }))
}

async fn stores(Path(path): Path<StorePath>, headers: HeaderMap) -> Response {
    tagged_reply(&headers, |etag| Stores {
        currency_code: CURRENCY_CODE,
        stores: [Store {
            metadata: HIDDEN_STORE_METADATA,
            slots: [Slot {
                metadata: HIDDEN_SLOT_METADATA,
                offers: [],
                slot_type: STATIC_SLOT,
            }],
            next_schedule_start_date: None,
            alias: path.alias.clone(),
        }],
        country_code: COUNTRY_CODE,
        etag,
    })
}

/// The path of a store call.
#[derive(Deserialize)]
struct StorePath {
    alias: String,
}

async fn entitlements<E: EntitlementStore>(
    State(shop): State<ShopState<E>>,
    Path(path): Path<PlayerPath>,
) -> Result<Response, ApiError> {
    let held = held_by(&shop, &path.persona).await?;
    let wallet = wallet_of(&shop, &path.persona).await?;
    let account = account_of(&path.persona).unwrap_or_default();
    let everyone = shop
        .table
        .held_by_all
        .iter()
        .map(|entitlement| HeldEntitlement::of(entitlement, account, STORE_START.to_owned()));
    let bought = held.iter().map(|got| {
        let name = shop.table.mount_of(&got.entitlement).map_or_else(
            || StoreEntitlement {
                entitlement: got.entitlement.clone(),
                name: got.entitlement.clone(),
            },
            |mount| mount.unlock.clone(),
        );
        HeldEntitlement::of(
            &name,
            got.instance,
            iso8601_millis(u64::try_from(got.granted_at_ms).unwrap_or_default()),
        )
    });
    Ok((
        [(CONTENT_TYPE, HeaderValue::from_static(JSON_CONTENT_TYPE))],
        axum::Json(Entitlements {
            has_more_results: false,
            entitlements: everyone
                .chain([
                    HeldEntitlement::wallet(wallet),
                    HeldEntitlement::transmog_tokens(),
                ])
                .chain(bought)
                .collect(),
            pagination_token: None,
        }),
    )
        .into_response())
}

/// `POST .../entitlements/sync`: retail answers `202` with no body.
async fn entitlements_sync() -> StatusCode {
    StatusCode::ACCEPTED
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, Bytes};
    use axum::http::Request;
    use nw_protocol::store_api::StoreMount;
    use serde_json::Value;

    use super::*;
    use crate::config::DEFAULT_MOUNT_PRICE;
    use crate::memory::MemoryEntitlements;

    const ACCOUNT: Uuid = Uuid::from_u128(0x5eed);
    const BEAR: &str = "Ent_Mount_MTX_7_bear_Ronin";
    const WOLF: &str = "Ent_Mount_MTX_1_wolf";
    const XPACK: &str = "Ent_Xpack_RiseOfTheAngryEarth_Owner";
    /// 2026-11-04 12:32:26 UTC, when the account got the bear.
    const BOUGHT_MS: i64 = 1_793_795_546_000;
    /// Length of a retail ETag: base64 of 32 bytes.
    const ETAG_LEN: usize = 44;
    /// The account's Marks of Fortune.
    const BALANCE: i64 = 13_000;
    /// A configured price and a configured price for the wolf alone.
    const MOUNT_PRICE: u32 = 15_000;
    const WOLF_PRICE: u32 = 10_000;

    fn player(persona: &str) -> String {
        format!("/players/{persona}/games/new-world/platforms/steam")
    }

    fn shop() -> ShopState<MemoryEntitlements> {
        shop_with(ShopConfig::default())
    }

    fn shop_with(settings: ShopConfig) -> ShopState<MemoryEntitlements> {
        let mount = |entitlement: &str, name: &str, mount: &str, product: &str| StoreMount {
            unlock: StoreEntitlement {
                entitlement: entitlement.to_owned(),
                name: name.to_owned(),
            },
            mount: mount.to_owned(),
            product: product.to_owned(),
        };
        let entitlements = MemoryEntitlements::default();
        entitlements.hold(
            ACCOUNT,
            AccountEntitlement {
                entitlement: BEAR.to_owned(),
                instance: Uuid::from_u128(7),
                granted_at_ms: BOUGHT_MS,
            },
        );
        entitlements.set_wallet(
            ACCOUNT,
            Wallet {
                marks: BALANCE,
                updated_at_ms: BOUGHT_MS,
            },
        );
        ShopState {
            table: Arc::new(StoreTable {
                mounts: vec![
                    mount(
                        BEAR,
                        "Ronin Bear Mount",
                        "Mount_MTX_7_bear_Ronin",
                        "Mount_Bear_MTX7_Ronin",
                    ),
                    mount(WOLF, "Ira", "Mount_MTX_1_wolf", "Mount_Wolf_1"),
                ],
                held_by_all: vec![StoreEntitlement {
                    entitlement: XPACK.to_owned(),
                    name: "Rise of the Angry Earth".to_owned(),
                }],
            }),
            settings: Arc::new(settings),
            entitlements,
        }
    }

    async fn send(method: &str, path: &str, etag: Option<&str>) -> (StatusCode, HeaderMap, Bytes) {
        send_to(shop(), method, path, etag).await
    }

    async fn send_to(
        shop: ShopState<MemoryEntitlements>,
        method: &str,
        path: &str,
        etag: Option<&str>,
    ) -> (StatusCode, HeaderMap, Bytes) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(etag) = etag {
            request = request.header(CLIENT_IF_NONE_MATCH, etag);
        }
        let response = tower::ServiceExt::oneshot(
            shop_router(shop),
            request.body(Body::empty()).expect("request"),
        )
        .await
        .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("body")
            .to_bytes();
        (status, headers, body)
    }

    async fn json(path: &str) -> Value {
        let (status, _, body) = send("GET", path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn the_catalog_offers_each_store_mount_for_free_and_counts_what_the_account_got() {
        let catalog = json(&format!(
            "{}/catalog?type=MTX",
            player(&ACCOUNT.to_string())
        ))
        .await;
        let offers = catalog["offers"].as_array().expect("offers");
        assert_eq!(offers.len(), 2);
        let bear = &offers[0];
        assert_eq!(bear["offerId"], BEAR);
        assert_eq!(bear["productId"], BEAR);
        assert_eq!(bear["alias"], "Mount_Bear_MTX7_Ronin", "its store product");
        assert_eq!(bear["name"], "Ronin Bear Mount");
        assert_eq!(bear["description"], "Ronin Bear Mount Durable");
        assert_eq!(bear["metadata"], MOUNT_OFFER_METADATA);
        assert_eq!(bear["offerType"], OFFER_TYPE);
        assert_eq!(bear["acquisitionCount"], 1, "the account got it");
        assert_eq!(bear["maxAcquisitionCount"], 1);
        assert_eq!(bear["entitlements"][0]["alias"], BEAR);
        assert_eq!(bear["entitlements"][0]["type"], DURABLE);
        let price = &bear["virtualCurrencyPrices"][0];
        assert_eq!(price["salesPriceWithDiscount"], 0, "the store is free");
        assert_eq!(price["originalPrice"], DEFAULT_MOUNT_PRICE);
        assert_eq!(price["discountPercentage"], FREE_DISCOUNT);
        assert_eq!(price["currencyId"], STORE_CURRENCY);
        assert_eq!(bear["inGameCurrencyPrices"][0]["salesPriceWithDiscount"], 0);
        assert_eq!(offers[1]["acquisitionCount"], 0, "not got yet");
        let stranger = json(&format!("{}/catalog?type=MTX", player("p"))).await;
        assert_eq!(stranger["offers"][0]["acquisitionCount"], 0);
    }

    async fn catalog_of(settings: ShopConfig) -> Value {
        let path = format!("{}/catalog?type=MTX", player(&ACCOUNT.to_string()));
        let (status, _, body) = send_to(shop_with(settings), "GET", &path, None).await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn a_store_that_is_not_free_charges_each_mount_its_price() {
        let catalog = catalog_of(ShopConfig {
            free: false,
            mount_price: MOUNT_PRICE,
            mount_prices: [(WOLF.to_owned(), WOLF_PRICE)].into(),
            ..ShopConfig::default()
        })
        .await;
        let prices: Vec<(u64, u64, u64)> = catalog["offers"]
            .as_array()
            .expect("offers")
            .iter()
            .map(|offer| {
                let price = &offer["virtualCurrencyPrices"][0];
                let number = |key: &str| price[key].as_u64().expect("number");
                (
                    number("salesPriceWithDiscount"),
                    number("originalPrice"),
                    number("discountPercentage"),
                )
            })
            .collect();
        assert_eq!(
            prices,
            [
                (u64::from(MOUNT_PRICE), u64::from(MOUNT_PRICE), 0),
                (u64::from(WOLF_PRICE), u64::from(WOLF_PRICE), 0),
            ]
        );
    }

    #[tokio::test]
    async fn a_closed_store_offers_nothing_and_the_account_keeps_what_it_got() {
        let settings = ShopConfig {
            enabled: false,
            ..ShopConfig::default()
        };
        let catalog = catalog_of(settings.clone()).await;
        assert_eq!(catalog["offers"], Value::Array(Vec::new()));
        let path = format!("{}/entitlements", player(&ACCOUNT.to_string()));
        let (_, _, body) = send_to(shop_with(settings), "GET", &path, None).await;
        let held: Value = serde_json::from_slice(&body).expect("json");
        assert!(
            held["entitlements"]
                .as_array()
                .expect("entitlements")
                .iter()
                .any(|entitlement| entitlement["alias"] == BEAR)
        );
    }

    #[tokio::test]
    async fn the_catalog_and_each_store_have_a_working_etag() {
        for (path, list) in [
            (format!("{}/catalog?type=MTX", player("p")), "offers"),
            (format!("{}/stores/Featured", player("p")), "stores"),
            (format!("{}/stores/Special1", player("p")), "stores"),
        ] {
            let (status, headers, body) = send("GET", &path, None).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            let value: Value = serde_json::from_slice(&body).expect("json");
            if list == "stores" {
                let alias = path.rsplit('/').next().expect("alias");
                assert_eq!(
                    value[list],
                    serde_json::json!([{
                        "metadata": HIDDEN_STORE_METADATA,
                        "slots": [{
                            "metadata": HIDDEN_SLOT_METADATA,
                            "offers": [],
                            "slotType": STATIC_SLOT,
                        }],
                        "nextScheduleStartDate": null,
                        "alias": alias,
                    }]),
                    "{path}"
                );
            }
            assert_eq!(value["currencyCode"], CURRENCY_CODE);
            assert_eq!(value["countryCode"], COUNTRY_CODE);
            let etag = headers[ETAG].to_str().expect("etag").to_owned();
            assert_eq!(etag.len(), ETAG_LEN);
            assert_eq!(value["etag"], etag.as_str());
            let (status, headers, body) = send("GET", &path, Some(&etag)).await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{path}");
            assert_eq!(headers[CONTENT_TYPE], NOT_MODIFIED_CONTENT_TYPE);
            assert!(body.is_empty());
            let (status, _, _) = send("GET", &path, Some("other")).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            let standard = Request::builder()
                .uri(&path)
                .header(IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .expect("request");
            let response = tower::ServiceExt::oneshot(shop_router(shop()), standard)
                .await
                .expect("response");
            assert_eq!(
                response.status(),
                StatusCode::NOT_MODIFIED,
                "If-None-Match too: {path}"
            );
        }
    }

    #[test]
    fn a_store_call_line_names_the_call_and_never_a_header_value() {
        let line = StoreCallLine {
            method: "GET",
            path: "/players/p/games/new-world/platforms/steam/catalog",
            query: "type=MTX",
            headers: vec!["host", "authorization"],
            status: 200,
        };
        assert_eq!(
            line.to_string(),
            "GET /players/p/games/new-world/platforms/steam/catalog?type=MTX \
             headers=host,authorization status=200"
        );
    }

    #[tokio::test]
    async fn the_entitlements_hold_the_expansion_and_what_the_account_got() {
        for persona in [
            ACCOUNT.to_string(),
            format!("amzn1.developerPersonaId.{ACCOUNT}"),
        ] {
            let (status, headers, body) =
                send("GET", &format!("{}/entitlements", player(&persona)), None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(headers[CONTENT_TYPE], JSON_CONTENT_TYPE);
            let value: Value = serde_json::from_slice(&body).expect("json");
            assert_eq!(
                value,
                serde_json::json!({
                    "hasMoreResults": false,
                    "entitlements": [
                        {
                            "name": "Rise of the Angry Earth",
                            "id": XPACK,
                            "type": DURABLE,
                            "alias": XPACK,
                            "amount": {
                                "instanceIds": [{ "id": ACCOUNT.to_string(), "metadata": {} }],
                                "totalAmount": 1
                            },
                            "lastUpdatedDate": STORE_START,
                            "externalItems": []
                        },
                        {
                            "name": STORE_CURRENCY_NAME,
                            "id": STORE_CURRENCY,
                            "type": WALLET,
                            "alias": STORE_CURRENCY,
                            "amount": {
                                "freeAmount": BALANCE,
                                "paidAmount": 0,
                                "pendingAmount": 0,
                                "lossAmount": 0,
                                "totalAmount": BALANCE
                            },
                            "lastUpdatedDate": "2026-11-04T12:32:26.000Z",
                            "externalItems": []
                        },
                        {
                            "name": TRANSMOG_TOKEN_NAME,
                            "id": TRANSMOG_TOKEN,
                            "type": CONSUMABLE,
                            "alias": TRANSMOG_TOKEN,
                            "amount": {
                                "freeAmount": TRANSMOG_TOKENS_HELD,
                                "paidAmount": 0,
                                "pendingAmount": 0,
                                "lossAmount": 0,
                                "totalAmount": TRANSMOG_TOKENS_HELD
                            },
                            "lastUpdatedDate": STORE_START,
                            "externalItems": []
                        },
                        {
                            "name": "Ronin Bear Mount",
                            "id": BEAR,
                            "type": DURABLE,
                            "alias": BEAR,
                            "amount": {
                                "instanceIds": [{
                                    "id": Uuid::from_u128(7).to_string(),
                                    "metadata": {}
                                }],
                                "totalAmount": 1
                            },
                            "lastUpdatedDate": "2026-11-04T12:32:26.000Z",
                            "externalItems": []
                        }
                    ],
                    "paginationToken": null
                }),
                "{persona}"
            );
        }
        let stranger = json(&format!("{}/entitlements", player("p"))).await;
        assert_eq!(
            stranger["entitlements"].as_array().map(Vec::len),
            Some(3),
            "the expansion, an empty wallet and the transmog tokens"
        );
        assert_eq!(stranger["entitlements"][1]["amount"]["totalAmount"], 0);
        let (status, _, body) =
            send("POST", &format!("{}/entitlements/sync", player("p")), None).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn other_games_and_paths_are_not_served() {
        for path in [
            "/players/p/games/other-game/platforms/steam/catalog",
            "/players/p/games/new-world/platforms/steam/purchases",
        ] {
            let (status, _, _) = send("GET", path, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }
}
