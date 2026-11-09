// SPDX-License-Identifier: AGPL-3.0-or-later
//! The social state of a world (world migration 0014): what the social service loads at start
//! and writes as it changes. The service keeps the whole state in memory; every change is one
//! small write here, and a treasury change is one transaction with its log row.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{DbError, is_unique_violation};

/// Bytes of a stored crest.
pub const CREST_LEN: usize = 36;
/// Bytes of a stored icon.
pub const ICON_LEN: usize = 9;
/// Bytes of the stored crossplay permissions.
pub const CROSSPLAY_LEN: usize = 10;
/// Bytes of the stored streamer-mode settings.
pub const STREAMER_MODE_LEN: usize = 2;
/// The window of the daily withdrawal limit: one day, in milliseconds.
pub const WITHDRAWAL_WINDOW_MS: u64 = 86_400_000;
/// Seconds per millisecond, for stored times.
const SECONDS_PER_MS: f64 = 0.001;
/// Milliseconds per second, for loaded times.
const MS_PER_SECOND: f64 = 1000.0;
/// The unique index of company names.
const COMPANY_NAME_INDEX: &str = "company_name_idx";
const LANGUAGE_FIELD: &str = "language";
pub(crate) const LEVEL_FIELD: &str = "level";
const FACTION_FIELD: &str = "faction";
const RANK_FIELD: &str = "rank";
const ICON_FIELD: &str = "icon";
const CREST_FIELD: &str = "crest";
const CROSSPLAY_FIELD: &str = "crossplay";
const STREAMER_MODE_FIELD: &str = "streamer_mode";
const TREASURY_FIELD: &str = "treasury";
const AMOUNT_FIELD: &str = "amount";
const COUNT_FIELD: &str = "count";

/// A link between two characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// A friend (stored both ways).
    Friend,
    /// A friend invite from the character to the other.
    Invite,
    /// The character blocks the other.
    Block,
    /// The character mutes the other's chat.
    Mute,
}

impl LinkKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Friend => "friend",
            Self::Invite => "invite",
            Self::Block => "block",
            Self::Mute => "mute",
        }
    }

    fn of_name(name: &str) -> Option<Self> {
        [Self::Friend, Self::Invite, Self::Block, Self::Mute]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

/// Why a treasury changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreasuryReason {
    /// A member put coin in.
    Deposit,
    /// A member took coin out.
    Withdraw,
    /// A withdrawal whose coin could not reach the member went back.
    Refund,
}

impl TreasuryReason {
    const fn name(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Withdraw => "withdraw",
            Self::Refund => "refund",
        }
    }
}

/// A character of the world, as other players see it.
#[derive(Debug, Clone, PartialEq)]
pub struct SocialCharacter {
    /// The character id.
    pub id: Uuid,
    /// The account.
    pub account: Uuid,
    /// The name.
    pub name: String,
    /// The client's create request body.
    pub appearance: serde_json::Value,
}

/// The stored options and offline look of a character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialSettings {
    /// The profanity filter is on.
    pub profanity_filter: bool,
    /// The client's language value.
    pub language: Option<u32>,
    /// The streamer-mode bytes.
    pub streamer_mode: [u8; STREAMER_MODE_LEN],
    /// The crossplay permission bytes.
    pub crossplay: Option<[u8; CROSSPLAY_LEN]>,
    /// The voice chat is on.
    pub voice_active: bool,
    /// The icon when last online.
    pub icon: Option<[u8; ICON_LEN]>,
    /// The level when last online.
    pub level: u32,
    /// The faction when last online.
    pub faction: u8,
    /// Unix milliseconds when last online.
    pub last_online_ms: Option<u64>,
    /// Unix milliseconds when it last left or lost its company.
    pub company_left_at_ms: Option<u64>,
}

impl Default for SocialSettings {
    /// A character that never set an option: the filter on, everything else unset.
    fn default() -> Self {
        Self {
            profanity_filter: true,
            language: None,
            streamer_mode: [0; STREAMER_MODE_LEN],
            crossplay: None,
            voice_active: false,
            icon: None,
            level: 0,
            faction: 0,
            last_online_ms: None,
            company_left_at_ms: None,
        }
    }
}

/// A company.
#[derive(Debug, Clone, PartialEq)]
pub struct Company {
    /// The company id.
    pub id: Uuid,
    /// The name.
    pub name: String,
    /// The faction.
    pub faction: u8,
    /// The crest bytes.
    pub crest: [u8; CREST_LEN],
    /// The message of the day.
    pub motd: String,
    /// Who wrote it.
    pub motd_author: Option<Uuid>,
    /// Unix milliseconds when it was written.
    pub motd_at_ms: Option<u64>,
    /// The hour its sieges start.
    pub siege_hour: f32,
    /// The treasury, hundredths of a coin.
    pub treasury: u64,
    /// The daily withdrawal limit; `None` for none.
    pub withdrawal_limit: Option<u64>,
    /// How often it was renamed.
    pub renames: u32,
    /// How often its crest changed.
    pub crest_changes: u32,
}

/// A company member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompanyMember {
    /// The character.
    pub character: Uuid,
    /// The company.
    pub company: Uuid,
    /// The rank, 0 to 3.
    pub rank: u8,
}

/// A company invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompanyInvite {
    /// The company.
    pub company: Uuid,
    /// The invitee.
    pub character: Uuid,
    /// Who sent it.
    pub inviter: Uuid,
}

/// A group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialGroup {
    /// The group id.
    pub id: Uuid,
    /// The leader.
    pub leader: Uuid,
    /// Its voice channel id.
    pub voice_channel: String,
    /// The members, in join order.
    pub members: Vec<Uuid>,
}

/// A player report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerReport {
    /// Who reports.
    pub reporter: Uuid,
    /// Who is reported.
    pub reported: Uuid,
    /// The reported name as the client sent it.
    pub reported_name: String,
    /// The client's reason value.
    pub reason: u32,
    /// The client's category value.
    pub category: u32,
    /// The player's comment.
    pub comment: String,
    /// The second text.
    pub detail: String,
    /// The client's locale.
    pub locale: String,
}

/// The whole social state of a world.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SocialRecords {
    /// Every character.
    pub characters: Vec<SocialCharacter>,
    /// The stored options, by character.
    pub settings: Vec<(Uuid, SocialSettings)>,
    /// Every link (character, other, kind).
    pub links: Vec<(Uuid, Uuid, LinkKind)>,
    /// Every company.
    pub companies: Vec<Company>,
    /// Every company member.
    pub members: Vec<CompanyMember>,
    /// Every company invite.
    pub invites: Vec<CompanyInvite>,
    /// Every group.
    pub groups: Vec<SocialGroup>,
    /// The treasury withdrawals and refunds of the last [`WITHDRAWAL_WINDOW_MS`]: character,
    /// Unix milliseconds, change.
    pub withdrawals: Vec<(Uuid, u64, i64)>,
    /// The coin owed to characters (world migration 0031).
    pub coin_returns: Vec<CoinReturn>,
}

/// Coin the social service owes a character (world migration 0031): a deposit the treasury did
/// not take, or a withdrawal it could not take back, while its sim did not take the coin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoinReturn {
    /// The id of the debt.
    pub id: Uuid,
    /// The character owed.
    pub character: Uuid,
    /// The company the coin came from or went to; `None` once it is deleted.
    pub company: Option<Uuid>,
    /// Hundredths of a coin.
    pub amount: u64,
}

/// A company as other players see it in a look-up (3610).
#[derive(Debug, Clone, PartialEq)]
pub struct CompanyListing {
    /// The company id.
    pub id: Uuid,
    /// The name.
    pub name: String,
    /// Its governor (the member of rank 0 who joined first); `None` for a company without one.
    pub governor: Option<Uuid>,
    /// The faction.
    pub faction: u8,
    /// The crest bytes.
    pub crest: [u8; CREST_LEN],
    /// Its member count.
    pub members: u32,
    /// The hour its sieges start.
    pub siege_hour: f32,
}

type SettingsRow = (
    Uuid,
    bool,
    Option<i64>,
    Vec<u8>,
    Option<Vec<u8>>,
    bool,
    Option<Vec<u8>>,
    i64,
    i16,
    Option<f64>,
    Option<f64>,
);
type CompanyRow = (
    Uuid,
    String,
    i16,
    Vec<u8>,
    String,
    Option<Uuid>,
    Option<f64>,
    f32,
    i64,
    Option<i64>,
    i32,
    i32,
);

fn ms_of(seconds: Option<f64>) -> Option<u64> {
    // A stored time is after 1970 and well inside `u64` milliseconds.
    seconds.map(|seconds| (seconds * MS_PER_SECOND) as u64)
}

fn seconds_of(ms: Option<u64>) -> Option<f64> {
    ms.map(|ms| ms as f64 * SECONDS_PER_MS)
}

fn fixed<const N: usize>(bytes: Vec<u8>, field: &'static str) -> Result<[u8; N], DbError> {
    bytes.try_into().map_err(|_| DbError::OutOfRange { field })
}

pub(crate) fn narrow<T: TryFrom<i64>>(value: i64, field: &'static str) -> Result<T, DbError> {
    T::try_from(value).map_err(|_| DbError::OutOfRange { field })
}

fn settings_of(row: SettingsRow) -> Result<(Uuid, SocialSettings), DbError> {
    let (id, filter, language, streamer, crossplay, voice, icon, level, faction, online, left) =
        row;
    Ok((
        id,
        SocialSettings {
            profanity_filter: filter,
            language: language
                .map(|value| narrow(value, LANGUAGE_FIELD))
                .transpose()?,
            streamer_mode: fixed(streamer, STREAMER_MODE_FIELD)?,
            crossplay: crossplay
                .map(|bytes| fixed(bytes, CROSSPLAY_FIELD))
                .transpose()?,
            voice_active: voice,
            icon: icon.map(|bytes| fixed(bytes, ICON_FIELD)).transpose()?,
            level: narrow(level, LEVEL_FIELD)?,
            faction: narrow(i64::from(faction), FACTION_FIELD)?,
            last_online_ms: ms_of(online),
            company_left_at_ms: ms_of(left),
        },
    ))
}

fn company_of(row: CompanyRow) -> Result<Company, DbError> {
    let (id, name, faction, crest, motd, author, motd_at, siege, treasury, limit, renames, crests) =
        row;
    Ok(Company {
        id,
        name,
        faction: narrow(i64::from(faction), FACTION_FIELD)?,
        crest: fixed(crest, CREST_FIELD)?,
        motd,
        motd_author: author,
        motd_at_ms: ms_of(motd_at),
        siege_hour: siege,
        treasury: narrow(treasury, TREASURY_FIELD)?,
        withdrawal_limit: limit
            .map(|limit| narrow(limit, TREASURY_FIELD))
            .transpose()?,
        renames: narrow(i64::from(renames), COUNT_FIELD)?,
        crest_changes: narrow(i64::from(crests), COUNT_FIELD)?,
    })
}

/// The columns of a [`CompanyRow`].
const COMPANY_COLUMNS: &str = "company_id, name, faction, crest, motd, motd_author, \
     extract(epoch FROM motd_at)::float8, siege_hour, treasury, withdrawal_limit, renames, \
     crest_changes";

/// The stored company `id`, as the database holds it; `None` when it has none.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails, and [`DbError::OutOfRange`] for a stored
/// value that does not fit its type.
pub async fn company_by_id(world: &PgPool, id: Uuid) -> Result<Option<Company>, DbError> {
    sqlx::query_as::<_, CompanyRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {COMPANY_COLUMNS} FROM company WHERE company_id = $1"
    )))
    .bind(id)
    .fetch_optional(world)
    .await?
    .map(company_of)
    .transpose()
}

/// The whole social state of the world.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails, and [`DbError::OutOfRange`] for a stored
/// value that does not fit its type.
pub async fn load_social(world: &PgPool) -> Result<SocialRecords, DbError> {
    let characters = sqlx::query_as::<_, (Uuid, Uuid, String, serde_json::Value)>(
        "SELECT character_id, account_id, name, appearance FROM player_character",
    )
    .fetch_all(world)
    .await?
    .into_iter()
    .map(|(id, account, name, appearance)| SocialCharacter {
        id,
        account,
        name,
        appearance,
    })
    .collect();
    let settings = sqlx::query_as::<_, SettingsRow>(
        "SELECT character_id, profanity_filter, language, streamer_mode, crossplay, \
         voice_active, icon, level, faction, extract(epoch FROM last_online)::float8, \
         extract(epoch FROM company_left_at)::float8 FROM character_social",
    )
    .fetch_all(world)
    .await?
    .into_iter()
    .map(settings_of)
    .collect::<Result<_, _>>()?;
    let links = sqlx::query_as::<_, (Uuid, Uuid, String)>(
        "SELECT character_id, other_id, kind FROM character_social_link",
    )
    .fetch_all(world)
    .await?
    .into_iter()
    .filter_map(|(character, other, kind)| {
        LinkKind::of_name(&kind).map(|kind| (character, other, kind))
    })
    .collect();
    let companies = sqlx::query_as::<_, CompanyRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {COMPANY_COLUMNS} FROM company"
    )))
    .fetch_all(world)
    .await?
    .into_iter()
    .map(company_of)
    .collect::<Result<_, _>>()?;
    let members = sqlx::query_as::<_, (Uuid, Uuid, i16)>(
        "SELECT character_id, company_id, rank FROM company_member ORDER BY joined_at",
    )
    .fetch_all(world)
    .await?
    .into_iter()
    .map(|(character, company, rank)| {
        Ok(CompanyMember {
            character,
            company,
            rank: narrow(i64::from(rank), RANK_FIELD)?,
        })
    })
    .collect::<Result<_, DbError>>()?;
    let invites = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "SELECT company_id, character_id, inviter_id FROM company_invite",
    )
    .fetch_all(world)
    .await?
    .into_iter()
    .map(|(company, character, inviter)| CompanyInvite {
        company,
        character,
        inviter,
    })
    .collect();
    let mut groups: Vec<SocialGroup> = sqlx::query_as::<_, (Uuid, Uuid, String)>(
        "SELECT group_id, leader_id, voice_channel FROM social_group",
    )
    .fetch_all(world)
    .await?
    .into_iter()
    .map(|(id, leader, voice_channel)| SocialGroup {
        id,
        leader,
        voice_channel,
        members: Vec::new(),
    })
    .collect();
    let group_members = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT character_id, group_id FROM social_group_member ORDER BY joined_at",
    )
    .fetch_all(world)
    .await?;
    for (character, group) in group_members {
        if let Some(group) = groups.iter_mut().find(|known| known.id == group) {
            group.members.push(character);
        }
    }
    let withdrawals = sqlx::query_as::<_, (Uuid, f64, i64)>(
        "SELECT character_id, extract(epoch FROM created_at)::float8, change \
         FROM company_treasury_log WHERE reason IN ('withdraw', 'refund') \
         AND created_at >= now() - make_interval(secs => $1)",
    )
    .bind(WITHDRAWAL_WINDOW_MS as f64 * SECONDS_PER_MS)
    .fetch_all(world)
    .await?
    .into_iter()
    .map(|(character, at, change)| (character, ms_of(Some(at)).unwrap_or_default(), change))
    .collect();
    let coin_returns = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>, i64)>(
        "SELECT return_id, character_id, company_id, amount FROM company_coin_return \
         ORDER BY created_at",
    )
    .fetch_all(world)
    .await?
    .into_iter()
    .map(|(id, character, company, amount)| {
        Ok(CoinReturn {
            id,
            character,
            company,
            amount: narrow(amount, AMOUNT_FIELD)?,
        })
    })
    .collect::<Result<_, DbError>>()?;
    Ok(SocialRecords {
        characters,
        settings,
        links,
        companies,
        members,
        invites,
        groups,
        withdrawals,
        coin_returns,
    })
}

/// One row of [`company_listings`]: id, name, governor, faction, crest, members, siege hour.
type CompanyListingRow = (Uuid, String, Option<Uuid>, i16, Vec<u8>, i64, f32);

/// The companies of `ids` that the world has, in no set order.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails, and [`DbError::OutOfRange`] for a stored
/// value that does not fit its type.
pub async fn company_listings(
    world: &PgPool,
    ids: &[Uuid],
) -> Result<Vec<CompanyListing>, DbError> {
    let rows: Vec<CompanyListingRow> = sqlx::query_as(
        "SELECT c.company_id, c.name, \
         (SELECT m.character_id FROM company_member m WHERE m.company_id = c.company_id \
          AND m.rank = 0 ORDER BY m.joined_at LIMIT 1), \
         c.faction, c.crest, \
         (SELECT COUNT(*) FROM company_member m WHERE m.company_id = c.company_id), \
         c.siege_hour FROM company c WHERE c.company_id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(world)
    .await?;
    rows.into_iter()
        .map(
            |(id, name, governor, faction, crest, members, siege_hour)| {
                Ok(CompanyListing {
                    id,
                    name,
                    governor,
                    faction: narrow(i64::from(faction), FACTION_FIELD)?,
                    crest: fixed(crest, CREST_FIELD)?,
                    members: narrow(members, COUNT_FIELD)?,
                    siege_hour,
                })
            },
        )
        .collect()
}

/// Stores the options and offline look of `character`.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails (also for a character the world does not
/// have).
pub async fn save_social_settings(
    world: &PgPool,
    character: Uuid,
    settings: &SocialSettings,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO character_social (character_id, profanity_filter, language, streamer_mode, \
         crossplay, voice_active, icon, level, faction, last_online, company_left_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, to_timestamp($10), to_timestamp($11)) \
         ON CONFLICT (character_id) DO UPDATE SET profanity_filter = $2, language = $3, \
         streamer_mode = $4, crossplay = $5, voice_active = $6, icon = $7, level = $8, \
         faction = $9, last_online = to_timestamp($10), company_left_at = to_timestamp($11)",
    )
    .bind(character)
    .bind(settings.profanity_filter)
    .bind(settings.language.map(i64::from))
    .bind(settings.streamer_mode.as_slice())
    .bind(settings.crossplay.as_ref().map(|bytes| bytes.to_vec()))
    .bind(settings.voice_active)
    .bind(settings.icon.as_ref().map(|bytes| bytes.to_vec()))
    .bind(i64::from(settings.level))
    .bind(i16::from(settings.faction))
    .bind(seconds_of(settings.last_online_ms))
    .bind(seconds_of(settings.company_left_at_ms))
    .execute(world)
    .await?;
    Ok(())
}

/// Adds a link (nothing when it exists).
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn add_social_link(
    world: &PgPool,
    character: Uuid,
    other: Uuid,
    kind: LinkKind,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO character_social_link (character_id, other_id, kind) VALUES ($1, $2, $3) \
         ON CONFLICT DO NOTHING",
    )
    .bind(character)
    .bind(other)
    .bind(kind.name())
    .execute(world)
    .await?;
    Ok(())
}

/// Removes a link (nothing when it does not exist).
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn remove_social_link(
    world: &PgPool,
    character: Uuid,
    other: Uuid,
    kind: LinkKind,
) -> Result<(), DbError> {
    sqlx::query(
        "DELETE FROM character_social_link WHERE character_id = $1 AND other_id = $2 \
         AND kind = $3",
    )
    .bind(character)
    .bind(other)
    .bind(kind.name())
    .execute(world)
    .await?;
    Ok(())
}

/// Stores a player report.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn insert_player_report(world: &PgPool, report: &PlayerReport) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO player_report (reporter_id, reported_id, reported_name, reason, category, \
         comment, detail, locale) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(report.reporter)
    .bind(report.reported)
    .bind(&report.reported_name)
    .bind(i64::from(report.reason))
    .bind(i64::from(report.category))
    .bind(&report.comment)
    .bind(&report.detail)
    .bind(&report.locale)
    .execute(world)
    .await?;
    Ok(())
}

/// The names of the characters and companies among `ids`, by id: what a leaderboard row shows
/// for its entity. An id of neither has no name.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn display_names(world: &PgPool, ids: &[Uuid]) -> Result<Vec<(Uuid, String)>, DbError> {
    let names: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT character_id, name FROM player_character WHERE character_id = ANY($1) \
         UNION ALL SELECT company_id, name FROM company WHERE company_id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(world)
    .await?;
    Ok(names)
}

/// Creates `company` with `founder` as its governor (rank 0). `Ok(false)` when another company
/// has the name (in any letter case).
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn create_company(
    world: &PgPool,
    company: &Company,
    founder: Uuid,
) -> Result<bool, DbError> {
    let mut transaction = world.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO company (company_id, name, faction, crest, siege_hour) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(company.id)
    .bind(&company.name)
    .bind(i16::from(company.faction))
    .bind(company.crest.as_slice())
    .bind(company.siege_hour)
    .execute(&mut *transaction)
    .await;
    match inserted {
        Err(error) if is_unique_violation(&error, COMPANY_NAME_INDEX) => return Ok(false),
        other => {
            other?;
        }
    }
    sqlx::query("INSERT INTO company_member (character_id, company_id, rank) VALUES ($1, $2, 0)")
        .bind(founder)
        .bind(company.id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(true)
}

/// Stores the editable fields of `company` (not its treasury). `Ok(false)` when its new name is
/// another company's.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn update_company(world: &PgPool, company: &Company) -> Result<bool, DbError> {
    let updated = sqlx::query(
        "UPDATE company SET name = $2, faction = $3, crest = $4, motd = $5, motd_author = $6, \
         motd_at = to_timestamp($7), siege_hour = $8, withdrawal_limit = $9, renames = $10, \
         crest_changes = $11 WHERE company_id = $1",
    )
    .bind(company.id)
    .bind(&company.name)
    .bind(i16::from(company.faction))
    .bind(company.crest.as_slice())
    .bind(&company.motd)
    .bind(company.motd_author)
    .bind(seconds_of(company.motd_at_ms))
    .bind(company.siege_hour)
    .bind(
        company
            .withdrawal_limit
            .map(|limit| i64::try_from(limit).unwrap_or(i64::MAX)),
    )
    .bind(i32::try_from(company.renames).unwrap_or(i32::MAX))
    .bind(i32::try_from(company.crest_changes).unwrap_or(i32::MAX))
    .execute(world)
    .await;
    match updated {
        Err(error) if is_unique_violation(&error, COMPANY_NAME_INDEX) => Ok(false),
        other => other.map(|_| true).map_err(DbError::from),
    }
}

/// Deletes a company with its members, invites and log.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn delete_company(world: &PgPool, company: Uuid) -> Result<(), DbError> {
    sqlx::query("DELETE FROM company WHERE company_id = $1")
        .bind(company)
        .execute(world)
        .await?;
    Ok(())
}

/// Puts `member` in its company with its rank, and removes its invites.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn set_company_member(world: &PgPool, member: &CompanyMember) -> Result<(), DbError> {
    let mut transaction = world.begin().await?;
    sqlx::query(
        "INSERT INTO company_member (character_id, company_id, rank) VALUES ($1, $2, $3) \
         ON CONFLICT (character_id) DO UPDATE SET company_id = $2, rank = $3",
    )
    .bind(member.character)
    .bind(member.company)
    .bind(i16::from(member.rank))
    .execute(&mut *transaction)
    .await?;
    sqlx::query("DELETE FROM company_invite WHERE character_id = $1")
        .bind(member.character)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// Takes `character` out of its company at Unix milliseconds `left_at_ms`.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn remove_company_member(
    world: &PgPool,
    character: Uuid,
    left_at_ms: u64,
) -> Result<(), DbError> {
    let mut transaction = world.begin().await?;
    sqlx::query("DELETE FROM company_member WHERE character_id = $1")
        .bind(character)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO character_social (character_id, company_left_at) \
         VALUES ($1, to_timestamp($2)) \
         ON CONFLICT (character_id) DO UPDATE SET company_left_at = to_timestamp($2)",
    )
    .bind(character)
    .bind(left_at_ms as f64 * SECONDS_PER_MS)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

/// Stores a company invite.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn add_company_invite(world: &PgPool, invite: &CompanyInvite) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO company_invite (company_id, character_id, inviter_id) VALUES ($1, $2, $3) \
         ON CONFLICT (company_id, character_id) DO UPDATE SET inviter_id = $3, created_at = now()",
    )
    .bind(invite.company)
    .bind(invite.character)
    .bind(invite.inviter)
    .execute(world)
    .await?;
    Ok(())
}

/// Removes a company invite.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn remove_company_invite(
    world: &PgPool,
    company: Uuid,
    character: Uuid,
) -> Result<(), DbError> {
    sqlx::query("DELETE FROM company_invite WHERE company_id = $1 AND character_id = $2")
        .bind(company)
        .bind(character)
        .execute(world)
        .await?;
    Ok(())
}

/// Adds `change` to the treasury of `company` for `character`, with its log row, in one
/// transaction. `Ok(None)` when the treasury holds less than a negative change takes; else the
/// balance after it.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails, and [`DbError::OutOfRange`] for a change
/// outside the column.
pub async fn change_treasury(
    world: &PgPool,
    company: Uuid,
    character: Uuid,
    change: i64,
    reason: TreasuryReason,
) -> Result<Option<u64>, DbError> {
    let mut transaction = world.begin().await?;
    let balance: Option<i64> = sqlx::query_scalar(
        "UPDATE company SET treasury = treasury + $2 \
         WHERE company_id = $1 AND treasury + $2 >= 0 RETURNING treasury",
    )
    .bind(company)
    .bind(change)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(balance) = balance else {
        return Ok(None);
    };
    sqlx::query(
        "INSERT INTO company_treasury_log (company_id, character_id, change, balance, reason) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(company)
    .bind(character)
    .bind(change)
    .bind(balance)
    .bind(reason.name())
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    narrow(balance, TREASURY_FIELD).map(Some)
}

/// Stores coin owed to a character (world migration 0031). A second store of the same id changes
/// nothing, so a repeated write is safe.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails, and [`DbError::OutOfRange`] for an amount
/// outside the column.
pub async fn add_coin_return(world: &PgPool, owed: &CoinReturn) -> Result<(), DbError> {
    let amount = i64::try_from(owed.amount).map_err(|_| DbError::OutOfRange {
        field: AMOUNT_FIELD,
    })?;
    sqlx::query(
        "INSERT INTO company_coin_return (return_id, character_id, company_id, amount) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (return_id) DO NOTHING",
    )
    .bind(owed.id)
    .bind(owed.character)
    .bind(owed.company)
    .bind(amount)
    .execute(world)
    .await?;
    Ok(())
}

/// Deletes the coin debt `id`: the character's sim took the coin.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn remove_coin_return(world: &PgPool, id: Uuid) -> Result<(), DbError> {
    sqlx::query("DELETE FROM company_coin_return WHERE return_id = $1")
        .bind(id)
        .execute(world)
        .await?;
    Ok(())
}

/// Stores a group with its leader and members.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn save_group(world: &PgPool, group: &SocialGroup) -> Result<(), DbError> {
    let mut transaction = world.begin().await?;
    sqlx::query(
        "INSERT INTO social_group (group_id, leader_id, voice_channel) VALUES ($1, $2, $3) \
         ON CONFLICT (group_id) DO UPDATE SET leader_id = $2",
    )
    .bind(group.id)
    .bind(group.leader)
    .bind(&group.voice_channel)
    .execute(&mut *transaction)
    .await?;
    sqlx::query("DELETE FROM social_group_member WHERE group_id = $1")
        .bind(group.id)
        .execute(&mut *transaction)
        .await?;
    for member in &group.members {
        sqlx::query(
            "INSERT INTO social_group_member (character_id, group_id) VALUES ($1, $2) \
             ON CONFLICT (character_id) DO UPDATE SET group_id = $2, joined_at = now()",
        )
        .bind(member)
        .bind(group.id)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

/// Deletes a group and its members.
///
/// # Errors
///
/// Returns [`DbError::Database`] when PostgreSQL fails.
pub async fn delete_group(world: &PgPool, group: Uuid) -> Result<(), DbError> {
    sqlx::query("DELETE FROM social_group WHERE group_id = $1")
        .bind(group)
        .execute(world)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_kinds_round_trip_their_names() {
        for kind in [
            LinkKind::Friend,
            LinkKind::Invite,
            LinkKind::Block,
            LinkKind::Mute,
        ] {
            assert_eq!(LinkKind::of_name(kind.name()), Some(kind));
        }
        assert_eq!(LinkKind::of_name("other"), None);
    }

    #[test]
    fn stored_times_round_trip_in_milliseconds() {
        const MS: u64 = 1_798_761_600_123;
        assert_eq!(ms_of(seconds_of(Some(MS))), Some(MS));
        assert_eq!(ms_of(None), None);
    }

    #[test]
    fn a_new_character_has_the_filter_on() {
        assert!(SocialSettings::default().profanity_filter);
    }
}

#[cfg(all(test, feature = "infra"))]
mod infra_tests {
    use super::*;
    use crate::migrate::WORLD_MIGRATOR;
    use crate::test_db::with_test_schema;
    use crate::{NewCharacter, insert_character};

    const DEPOSIT: i64 = 50_000;

    async fn character(pool: &PgPool, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        insert_character(
            pool,
            &NewCharacter {
                id,
                account_id: Uuid::new_v4(),
                name: name.to_owned(),
                appearance: serde_json::json!({}),
            },
        )
        .await
        .expect("insert character");
        id
    }

    fn company(name: &str) -> Company {
        Company {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            faction: 1,
            crest: [0; CREST_LEN],
            motd: String::new(),
            motd_author: None,
            motd_at_ms: None,
            siege_hour: 18.0,
            treasury: 0,
            withdrawal_limit: None,
            renames: 0,
            crest_changes: 0,
        }
    }

    #[tokio::test]
    async fn display_names_cover_characters_and_companies() {
        let (names, ann, alpha) = with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let ann = character(pool, "Ann").await;
            let alpha = company("Alpha");
            assert!(create_company(pool, &alpha, ann).await.expect("create"));
            let mut names = display_names(pool, &[ann, alpha.id, Uuid::new_v4()])
                .await
                .expect("names");
            names.sort();
            (names, ann, alpha.id)
        })
        .await;
        let mut expected = vec![(ann, "Ann".to_owned()), (alpha, "Alpha".to_owned())];
        expected.sort();
        assert_eq!(names, expected);
    }

    #[tokio::test]
    async fn the_social_state_round_trips() {
        with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let ann = character(pool, "Ann").await;
            let bob = character(pool, "Bob").await;
            let settings = SocialSettings {
                profanity_filter: false,
                language: Some(8),
                icon: Some([1; ICON_LEN]),
                level: 42,
                faction: 2,
                last_online_ms: Some(1_798_761_600_000),
                ..SocialSettings::default()
            };
            save_social_settings(pool, ann, &settings)
                .await
                .expect("save");
            add_social_link(pool, ann, bob, LinkKind::Friend)
                .await
                .expect("link");
            add_social_link(pool, ann, bob, LinkKind::Block)
                .await
                .expect("link");
            remove_social_link(pool, ann, bob, LinkKind::Block)
                .await
                .expect("unlink");
            let first = company("Alpha");
            assert!(create_company(pool, &first, ann).await.expect("create"));
            assert!(
                !create_company(pool, &company("ALPHA"), bob)
                    .await
                    .expect("taken")
            );
            add_company_invite(
                pool,
                &CompanyInvite {
                    company: first.id,
                    character: bob,
                    inviter: ann,
                },
            )
            .await
            .expect("invite");
            set_company_member(
                pool,
                &CompanyMember {
                    character: bob,
                    company: first.id,
                    rank: 3,
                },
            )
            .await
            .expect("join");
            assert_eq!(
                change_treasury(pool, first.id, ann, DEPOSIT, TreasuryReason::Deposit)
                    .await
                    .expect("treasury"),
                Some(DEPOSIT as u64)
            );
            assert_eq!(
                change_treasury(pool, first.id, bob, -2 * DEPOSIT, TreasuryReason::Withdraw)
                    .await
                    .expect("treasury"),
                None
            );
            assert_eq!(
                change_treasury(pool, first.id, bob, -DEPOSIT, TreasuryReason::Withdraw)
                    .await
                    .expect("treasury"),
                Some(0)
            );
            assert_eq!(
                company_by_id(pool, first.id)
                    .await
                    .expect("read")
                    .map(|stored| stored.treasury),
                Some(0)
            );
            assert_eq!(
                company_by_id(pool, Uuid::new_v4()).await.expect("read"),
                None
            );
            let listed = company_listings(pool, &[first.id]).await.expect("listings");
            assert_eq!(listed[0].governor, Some(ann));
            assert_eq!(listed[0].members, 2);
            save_group(
                pool,
                &SocialGroup {
                    id: Uuid::new_v4(),
                    leader: ann,
                    voice_channel: "g".to_owned(),
                    members: vec![ann, bob],
                },
            )
            .await
            .expect("group");
            let records = load_social(pool).await.expect("load");
            assert_eq!(records.characters.len(), 2);
            assert_eq!(records.settings, vec![(ann, settings)]);
            assert_eq!(records.links, vec![(ann, bob, LinkKind::Friend)]);
            assert_eq!(records.companies.len(), 1);
            assert_eq!(records.members.len(), 2);
            assert!(records.invites.is_empty(), "joining ends the invite");
            assert_eq!(records.groups[0].members, vec![ann, bob]);
            assert_eq!(records.withdrawals.len(), 1);
            assert_eq!(records.withdrawals[0].2, -DEPOSIT);
            remove_company_member(pool, bob, 1_798_761_600_000)
                .await
                .expect("leave");
            delete_company(pool, first.id).await.expect("delete");
            let records = load_social(pool).await.expect("load");
            assert!(records.companies.is_empty() && records.members.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_coin_debt_is_stored_once_and_ends_when_paid() {
        with_test_schema(async |pool| {
            WORLD_MIGRATOR.run(pool).await.expect("migrate");
            let ann = character(pool, "Ann").await;
            let first = company("Alpha");
            assert!(create_company(pool, &first, ann).await.expect("create"));
            let owed = CoinReturn {
                id: Uuid::new_v4(),
                character: ann,
                company: Some(first.id),
                amount: DEPOSIT as u64,
            };
            add_coin_return(pool, &owed).await.expect("owe");
            add_coin_return(pool, &owed).await.expect("owe again");
            assert_eq!(
                load_social(pool).await.expect("load").coin_returns,
                vec![owed]
            );
            delete_company(pool, first.id).await.expect("delete");
            assert_eq!(
                load_social(pool).await.expect("load").coin_returns,
                vec![CoinReturn {
                    company: None,
                    ..owed
                }],
                "the debt outlives its company"
            );
            remove_coin_return(pool, owed.id).await.expect("paid");
            assert!(
                load_social(pool)
                    .await
                    .expect("load")
                    .coin_returns
                    .is_empty()
            );
        })
        .await;
    }
}
