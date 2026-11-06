// SPDX-License-Identifier: AGPL-3.0-or-later
//! In-memory stores for tests. They follow the same contract as the PostgreSQL and Valkey
//! stores; [`contract`] checks both.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nw_db::{
    Account, AccountEntitlement, AnalyticsRecord, BoardEntry, BoardOrder, BoardPage, Character,
    DiscordProfile, NewCharacter, PrivacySetting, StatValues, Wallet,
};
use nw_protocol::launcher_api::LauncherSession;
use uuid::Uuid;

use crate::queue::{
    ENTRY_TICKET_SECONDS, EntryRecord, QueueAction, QueueSettings, QueueStatus, QueueStore,
    QueueTime,
};
use crate::store::{
    AccountStore, AnalyticsStore, CharacterStore, DiscordAttempt, DiscordStateStore,
    EntitlementStore, GameSessionStore, PrivacyStore, RateLimiter, StatsStore, StoreError,
};
use crate::time::MILLIS_PER_SECOND;
use nw_common::time::{unix_ms, unix_seconds};

/// The level (zero-based) of a character that was never in game.
const NEW_CHARACTER_LEVEL: u32 = 0;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct Accounts {
    accounts: HashMap<Uuid, Account>,
    passwords: HashMap<String, (Uuid, String)>,
    discord: HashMap<String, (Uuid, DiscordProfile)>,
    sessions: HashMap<[u8; 32], (Uuid, i64)>,
}

/// Accounts in memory.
#[derive(Clone, Default)]
pub(crate) struct MemoryAccounts(Arc<Mutex<Accounts>>);

impl MemoryAccounts {
    /// Sets the ban flag of an account.
    pub(crate) fn set_banned(&self, account_id: Uuid, banned: bool) {
        if let Some(account) = lock(&self.0).accounts.get_mut(&account_id) {
            account.banned = banned;
        }
    }

    /// The newest Discord profile stored for a Discord user id.
    pub(crate) fn discord_profile(&self, discord_user_id: &str) -> Option<DiscordProfile> {
        lock(&self.0)
            .discord
            .get(discord_user_id)
            .map(|(_, profile)| profile.clone())
    }
}

fn new_account(accounts: &mut Accounts, display_name: &str) -> Account {
    let account = Account {
        id: Uuid::new_v4(),
        display_name: display_name.to_owned(),
        created_at_unix: unix_seconds(),
        banned: false,
    };
    accounts.accounts.insert(account.id, account.clone());
    account
}

impl AccountStore for MemoryAccounts {
    async fn create_password_account(
        &self,
        username: &str,
        password_hash: &str,
    ) -> Result<Account, StoreError> {
        let mut accounts = lock(&self.0);
        if accounts.passwords.contains_key(username) {
            return Err(StoreError::DuplicateUsername);
        }
        let account = new_account(&mut accounts, username);
        accounts
            .passwords
            .insert(username.to_owned(), (account.id, password_hash.to_owned()));
        Ok(account)
    }

    async fn password_login(
        &self,
        username: &str,
    ) -> Result<Option<(Account, String)>, StoreError> {
        let accounts = lock(&self.0);
        Ok(accounts.passwords.get(username).and_then(|(id, hash)| {
            accounts
                .accounts
                .get(id)
                .map(|account| (account.clone(), hash.clone()))
        }))
    }

    async fn discord_account(&self, profile: &DiscordProfile) -> Result<Account, StoreError> {
        let mut accounts = lock(&self.0);
        let known = accounts
            .discord
            .get(&profile.user_id)
            .and_then(|(id, _)| accounts.accounts.get(id))
            .cloned();
        let account = known.unwrap_or_else(|| new_account(&mut accounts, &profile.display_name));
        accounts
            .discord
            .insert(profile.user_id.clone(), (account.id, profile.clone()));
        Ok(account)
    }

    async fn discord_user_id(&self, account_id: Uuid) -> Result<Option<String>, StoreError> {
        Ok(lock(&self.0)
            .discord
            .iter()
            .find(|(_, (id, _))| *id == account_id)
            .map(|(user_id, _)| user_id.clone()))
    }

    async fn create_launcher_session(
        &self,
        token_hash: [u8; 32],
        account_id: Uuid,
        expires_at: i64,
    ) -> Result<(), StoreError> {
        lock(&self.0)
            .sessions
            .insert(token_hash, (account_id, expires_at));
        Ok(())
    }

    async fn launcher_session_account(
        &self,
        token_hash: [u8; 32],
    ) -> Result<Option<Account>, StoreError> {
        let accounts = lock(&self.0);
        Ok(accounts
            .sessions
            .get(&token_hash)
            .filter(|(_, expires_at)| *expires_at > unix_seconds())
            .and_then(|(id, _)| accounts.accounts.get(id).cloned()))
    }

    async fn delete_launcher_session(&self, token_hash: [u8; 32]) -> Result<(), StoreError> {
        lock(&self.0).sessions.remove(&token_hash);
        Ok(())
    }
}

/// A value that expires.
struct Expiring<T> {
    value: T,
    until: Instant,
}

impl<T> Expiring<T> {
    fn new(value: T, seconds: u64) -> Self {
        Self {
            value,
            until: Instant::now() + Duration::from_secs(seconds),
        }
    }

    fn live(&self) -> bool {
        Instant::now() < self.until
    }
}

/// Game session keys in memory.
#[derive(Clone, Default)]
pub(crate) struct MemoryGameSessions(Arc<Mutex<HashMap<String, Expiring<Uuid>>>>);

impl GameSessionStore for MemoryGameSessions {
    async fn create(
        &self,
        key: &str,
        account_id: Uuid,
        claim_seconds: u64,
    ) -> Result<(), StoreError> {
        lock(&self.0).insert(key.to_owned(), Expiring::new(account_id, claim_seconds));
        Ok(())
    }

    async fn resolve(&self, key: &str, ttl_seconds: u64) -> Result<Option<Uuid>, StoreError> {
        let mut sessions = lock(&self.0);
        let Some(entry) = sessions.get(key).filter(|entry| entry.live()) else {
            sessions.remove(key);
            return Ok(None);
        };
        let account_id = entry.value;
        sessions.insert(key.to_owned(), Expiring::new(account_id, ttl_seconds));
        Ok(Some(account_id))
    }

    async fn revoke(&self, key: &str) -> Result<(), StoreError> {
        lock(&self.0).remove(key);
        Ok(())
    }
}

#[derive(Default)]
struct Characters {
    worlds: HashMap<Uuid, Vec<Character>>,
    names: HashMap<String, Uuid>,
    banned: std::collections::HashSet<Uuid>,
}

/// Characters and names in memory.
#[derive(Clone)]
pub(crate) struct MemoryCharacters(Arc<Mutex<Characters>>);

impl MemoryCharacters {
    /// A store with these worlds and no characters.
    pub(crate) fn new(world_ids: &[Uuid]) -> Self {
        let worlds = world_ids.iter().map(|id| (*id, Vec::new())).collect();
        Self(Arc::new(Mutex::new(Characters {
            worlds,
            names: HashMap::new(),
            banned: std::collections::HashSet::new(),
        })))
    }

    /// Bans `account`.
    pub(crate) fn ban(&self, account: Uuid) {
        lock(&self.0).banned.insert(account);
    }
}

impl CharacterStore for MemoryCharacters {
    async fn characters(
        &self,
        world_id: Uuid,
        account_id: Uuid,
    ) -> Result<Vec<Character>, StoreError> {
        let characters = lock(&self.0);
        let world = characters
            .worlds
            .get(&world_id)
            .ok_or(StoreError::UnknownWorld)?;
        Ok(world
            .iter()
            .filter(|character| character.account_id == account_id)
            .cloned()
            .collect())
    }

    async fn character(
        &self,
        world_id: Uuid,
        character_id: Uuid,
    ) -> Result<Option<Character>, StoreError> {
        let characters = lock(&self.0);
        let world = characters
            .worlds
            .get(&world_id)
            .ok_or(StoreError::UnknownWorld)?;
        Ok(world
            .iter()
            .find(|character| character.id == character_id)
            .cloned())
    }

    async fn name_taken(&self, name: &str) -> Result<bool, StoreError> {
        Ok(lock(&self.0).names.contains_key(&name.to_lowercase()))
    }

    async fn account_banned(&self, account: Uuid) -> Result<bool, StoreError> {
        Ok(lock(&self.0).banned.contains(&account))
    }

    async fn create(&self, world_id: Uuid, new: &NewCharacter) -> Result<Character, StoreError> {
        let mut characters = lock(&self.0);
        if !characters.worlds.contains_key(&world_id) {
            return Err(StoreError::UnknownWorld);
        }
        let name_key = new.name.to_lowercase();
        if characters.names.contains_key(&name_key) {
            return Err(StoreError::DuplicateCharacterName);
        }
        let character = Character {
            id: new.id,
            account_id: new.account_id,
            name: new.name.clone(),
            appearance: new.appearance.clone(),
            created_at_unix: unix_seconds(),
            level: NEW_CHARACTER_LEVEL,
            published_look: None,
        };
        characters.names.insert(name_key, new.id);
        characters
            .worlds
            .entry(world_id)
            .or_default()
            .push(character.clone());
        Ok(character)
    }

    async fn delete(
        &self,
        world_id: Uuid,
        account_id: Uuid,
        character_id: Uuid,
    ) -> Result<bool, StoreError> {
        let mut characters = lock(&self.0);
        let world = characters
            .worlds
            .get_mut(&world_id)
            .ok_or(StoreError::UnknownWorld)?;
        let Some(at) = world.iter().position(|character| {
            character.id == character_id && character.account_id == account_id
        }) else {
            return Ok(false);
        };
        let character = world.remove(at);
        characters.names.remove(&character.name.to_lowercase());
        Ok(true)
    }
}

/// Rate-limit counters in memory.
#[derive(Clone, Default)]
pub(crate) struct MemoryRateLimiter(Arc<Mutex<HashMap<String, Expiring<u64>>>>);

impl RateLimiter for MemoryRateLimiter {
    async fn increment(&self, key: &str, window_seconds: u64) -> Result<u64, StoreError> {
        let mut counters = lock(&self.0);
        match counters.get_mut(key).filter(|entry| entry.live()) {
            Some(entry) => {
                entry.value += 1;
                Ok(entry.value)
            }
            None => {
                counters.insert(key.to_owned(), Expiring::new(1, window_seconds));
                Ok(1)
            }
        }
    }

    async fn release(&self, key: &str) -> Result<(), StoreError> {
        if let Some(entry) = lock(&self.0).get_mut(key).filter(|entry| entry.live()) {
            entry.value = entry.value.saturating_sub(1);
        }
        Ok(())
    }
}

#[derive(Default)]
struct DiscordStates {
    verifiers: HashMap<String, Expiring<String>>,
    results: HashMap<String, Expiring<LauncherSession>>,
}

/// Discord login attempts in memory.
#[derive(Clone, Default)]
pub(crate) struct MemoryDiscordStates(Arc<Mutex<DiscordStates>>);

impl DiscordStateStore for MemoryDiscordStates {
    async fn begin(&self, state: &str, verifier: &str, ttl_seconds: u64) -> Result<(), StoreError> {
        lock(&self.0).verifiers.insert(
            state.to_owned(),
            Expiring::new(verifier.to_owned(), ttl_seconds),
        );
        Ok(())
    }

    async fn take_verifier(&self, state: &str) -> Result<Option<String>, StoreError> {
        Ok(lock(&self.0)
            .verifiers
            .get_mut(state)
            .filter(|entry| entry.live() && !entry.value.is_empty())
            .map(|entry| std::mem::take(&mut entry.value)))
    }

    async fn complete(
        &self,
        state: &str,
        session: &LauncherSession,
        ttl_seconds: u64,
    ) -> Result<(), StoreError> {
        lock(&self.0).results.insert(
            state.to_owned(),
            Expiring::new(session.clone(), ttl_seconds),
        );
        Ok(())
    }

    async fn poll(&self, state: &str) -> Result<DiscordAttempt, StoreError> {
        let mut states = lock(&self.0);
        if let Some(result) = states.results.remove(state).filter(|entry| entry.live()) {
            states.verifiers.remove(state);
            return Ok(DiscordAttempt::Ready(result.value));
        }
        if states
            .verifiers
            .get(state)
            .is_some_and(|entry| entry.live())
        {
            Ok(DiscordAttempt::Pending)
        } else {
            Ok(DiscordAttempt::Unknown)
        }
    }
}

#[derive(Debug, Clone)]
struct MemoryEntry {
    account_id: Uuid,
    character_id: Uuid,
    joined_ms: u64,
    refreshed_ms: u64,
}

#[derive(Default)]
struct WorldQueue {
    waiting: HashMap<String, MemoryEntry>,
    admitted: HashMap<String, (EntryRecord, u64)>,
    accounts: HashMap<Uuid, String>,
    /// Free player slots and their end, Unix ms of the real clock (like the Valkey key expiry).
    free_slots: Option<(u32, u64)>,
}

/// Login queues in memory, with the rules of the Valkey script.
#[derive(Clone, Default)]
pub(crate) struct MemoryQueue(Arc<Mutex<HashMap<Uuid, WorldQueue>>>);

impl WorldQueue {
    fn forget(&mut self, ticket: &str) {
        self.waiting.remove(ticket);
        self.admitted.remove(ticket);
        self.accounts.retain(|_, owned| owned != ticket);
    }

    fn order(&self) -> Vec<String> {
        let mut tickets: Vec<(&String, u64)> = self
            .waiting
            .iter()
            .map(|(ticket, entry)| (ticket, entry.joined_ms))
            .collect();
        tickets.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(right.0)));
        tickets
            .into_iter()
            .map(|(ticket, _)| ticket.clone())
            .collect()
    }
}

impl QueueStore for MemoryQueue {
    async fn update(
        &self,
        settings: &QueueSettings,
        account_id: Uuid,
        action: QueueAction,
        time: QueueTime,
    ) -> Result<Option<QueueStatus>, StoreError> {
        let now_ms = match time {
            QueueTime::StoreClock => unix_ms(),
            QueueTime::At(ms) => ms,
        };
        let mut worlds = lock(&self.0);
        let queue = worlds.entry(settings.world_id).or_default();
        let stale_before = now_ms.saturating_sub(settings.stale_after_ms);
        let stale: Vec<String> = queue
            .waiting
            .iter()
            .filter(|(_, entry)| entry.refreshed_ms < stale_before)
            .map(|(ticket, _)| ticket.clone())
            .collect();
        for ticket in stale {
            queue.forget(&ticket);
        }
        queue
            .admitted
            .retain(|_, (_, expires_ms)| *expires_ms > now_ms);
        let ticket = match action {
            QueueAction::Join {
                character_id,
                new_ticket_id,
            } => {
                let existing = queue.accounts.get(&account_id).cloned();
                let reuse = existing.as_ref().filter(|ticket| {
                    queue
                        .waiting
                        .get(*ticket)
                        .map(|entry| entry.character_id)
                        .or_else(|| {
                            queue
                                .admitted
                                .get(*ticket)
                                .map(|(record, _)| record.character_id)
                        })
                        == Some(character_id)
                });
                match reuse {
                    Some(ticket) => ticket.clone(),
                    None => {
                        if let Some(old) = existing {
                            queue.forget(&old);
                        }
                        queue.waiting.insert(
                            new_ticket_id.clone(),
                            MemoryEntry {
                                account_id,
                                character_id,
                                joined_ms: now_ms,
                                refreshed_ms: now_ms,
                            },
                        );
                        queue.accounts.insert(account_id, new_ticket_id.clone());
                        new_ticket_id
                    }
                }
            }
            QueueAction::Refresh { ticket_id } => {
                let owner = queue
                    .waiting
                    .get(&ticket_id)
                    .map(|entry| entry.account_id)
                    .or_else(|| {
                        queue
                            .admitted
                            .get(&ticket_id)
                            .map(|(record, _)| record.account_id)
                    });
                if owner != Some(account_id) {
                    return Ok(None);
                }
                ticket_id
            }
        };
        if let Some(entry) = queue.waiting.get_mut(&ticket) {
            entry.refreshed_ms = now_ms;
        }
        let free_slots = queue
            .free_slots
            .filter(|(_, ends_ms)| *ends_ms > unix_ms())
            .map_or(0, |(slots, _)| slots);
        let limit = settings.capacity.min(free_slots) as usize;
        for head in queue.order() {
            if queue.admitted.len() >= limit {
                break;
            }
            if let Some(entry) = queue.waiting.remove(&head) {
                let expires_ms = now_ms + ENTRY_TICKET_SECONDS * MILLIS_PER_SECOND;
                let record = EntryRecord {
                    ticket_id: head.clone(),
                    account_id: entry.account_id,
                    world_id: settings.world_id,
                    character_id: entry.character_id,
                    expires_at: i64::try_from(expires_ms / MILLIS_PER_SECOND).unwrap_or(i64::MAX),
                };
                queue.admitted.insert(head, (record, expires_ms));
            }
        }
        if let Some((record, _)) = queue.admitted.get(&ticket) {
            return Ok(Some(QueueStatus::Admitted(record.clone())));
        }
        let position = queue.order().iter().position(|waiting| *waiting == ticket);
        Ok(position.map(|index| QueueStatus::Waiting {
            ticket_id: ticket,
            position: index as u64 + 1,
        }))
    }

    async fn set_free_slots(
        &self,
        world_id: Uuid,
        slots: u32,
        ttl_ms: u64,
    ) -> Result<(), StoreError> {
        lock(&self.0).entry(world_id).or_default().free_slots =
            Some((slots, unix_ms().saturating_add(ttl_ms)));
        Ok(())
    }

    async fn free_slots(&self, world_id: Uuid) -> Result<u32, StoreError> {
        Ok(lock(&self.0)
            .get(&world_id)
            .and_then(|queue| queue.free_slots)
            .filter(|(_, ends_ms)| *ends_ms > unix_ms())
            .map_or(0, |(slots, _)| slots))
    }

    async fn take_entry(&self, ticket_id: &str) -> Result<Option<EntryRecord>, StoreError> {
        let mut worlds = lock(&self.0);
        for queue in worlds.values_mut() {
            if let Some((record, _)) = queue.admitted.remove(ticket_id) {
                queue.accounts.retain(|_, owned| owned != ticket_id);
                return Ok(Some(record));
            }
        }
        Ok(None)
    }
}

/// A page request: namespace, board, order, page number, page size.
pub(crate) type PageRequest = (String, String, BoardOrder, u32, u32);
/// A stat values request: scope, stat ids, entity ids.
pub(crate) type ValuesRequest = (String, Vec<String>, Vec<String>);

#[derive(Default)]
struct Stats {
    boards: HashMap<String, BoardPage>,
    values: StatValues,
    last_page: Option<PageRequest>,
    last_values: Option<ValuesRequest>,
}

/// The store of each account in memory: its entitlements and its wallet.
#[derive(Default)]
struct AccountStores {
    held: HashMap<Uuid, Vec<AccountEntitlement>>,
    wallets: HashMap<Uuid, Wallet>,
}

/// Store entitlements and wallets in memory, by account.
#[derive(Clone, Default)]
pub(crate) struct MemoryEntitlements(Arc<Mutex<AccountStores>>);

impl MemoryEntitlements {
    /// `account` holds `held`.
    pub(crate) fn hold(&self, account: Uuid, held: AccountEntitlement) {
        lock(&self.0).held.entry(account).or_default().push(held);
    }

    /// The wallet of `account` is `wallet`.
    pub(crate) fn set_wallet(&self, account: Uuid, wallet: Wallet) {
        lock(&self.0).wallets.insert(account, wallet);
    }
}

impl EntitlementStore for MemoryEntitlements {
    async fn held(&self, account: Uuid) -> Result<Vec<AccountEntitlement>, StoreError> {
        Ok(lock(&self.0)
            .held
            .get(&account)
            .cloned()
            .unwrap_or_default())
    }

    async fn wallet(&self, account: Uuid) -> Result<Wallet, StoreError> {
        Ok(lock(&self.0)
            .wallets
            .get(&account)
            .copied()
            .unwrap_or_default())
    }
}

/// Stats in memory: set boards and values answer every request, and the last requests are kept.
#[derive(Clone, Default)]
pub(crate) struct MemoryStats(Arc<Mutex<Stats>>);

impl MemoryStats {
    /// Board `board_id` answers every page request with `page`.
    pub(crate) fn set_board(&self, board_id: &str, page: BoardPage) {
        lock(&self.0).boards.insert(board_id.to_owned(), page);
    }

    /// Every stat values request gets `values`.
    pub(crate) fn set_values(&self, values: StatValues) {
        lock(&self.0).values = values;
    }

    /// The last page request.
    pub(crate) fn last_page(&self) -> Option<PageRequest> {
        lock(&self.0).last_page.clone()
    }

    /// The last stat values request.
    pub(crate) fn last_values_query(&self) -> Option<ValuesRequest> {
        lock(&self.0).last_values.clone()
    }
}

impl StatsStore for MemoryStats {
    async fn leaderboard_page(
        &self,
        namespace: &str,
        board_id: &str,
        order: BoardOrder,
        page_number: u32,
        page_size: u32,
    ) -> Result<BoardPage, StoreError> {
        let mut stats = lock(&self.0);
        stats.last_page = Some((
            namespace.to_owned(),
            board_id.to_owned(),
            order,
            page_number,
            page_size,
        ));
        Ok(stats.boards.get(board_id).cloned().unwrap_or(BoardPage {
            entries: Vec::new(),
            size: 0,
        }))
    }

    async fn leaderboard_entries(
        &self,
        _namespace: &str,
        board_id: &str,
        _order: BoardOrder,
        entity_ids: &[String],
    ) -> Result<Vec<BoardEntry>, StoreError> {
        Ok(lock(&self.0)
            .boards
            .get(board_id)
            .map(|page| {
                page.entries
                    .iter()
                    .filter(|entry| entity_ids.contains(&entry.entity_id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn stat_values(
        &self,
        _namespace: &str,
        scope: &str,
        stat_ids: &[String],
        entity_ids: &[String],
    ) -> Result<StatValues, StoreError> {
        let mut stats = lock(&self.0);
        stats.last_values = Some((scope.to_owned(), stat_ids.to_vec(), entity_ids.to_vec()));
        Ok(stats.values.clone())
    }
}

/// The analytics stream in memory.
#[derive(Clone, Default)]
pub(crate) struct MemoryAnalytics(Arc<Mutex<Vec<(String, AnalyticsRecord)>>>);

impl MemoryAnalytics {
    /// Every stored record with its stream.
    pub(crate) fn records(&self) -> Vec<(String, AnalyticsRecord)> {
        lock(&self.0).clone()
    }
}

impl AnalyticsStore for MemoryAnalytics {
    async fn put_records(
        &self,
        stream_name: &str,
        records: &[AnalyticsRecord],
    ) -> Result<Vec<i64>, StoreError> {
        let mut stored = lock(&self.0);
        let first = i64::try_from(stored.len()).unwrap_or(i64::MAX);
        stored.extend(
            records
                .iter()
                .map(|record| (stream_name.to_owned(), record.clone())),
        );
        Ok((first..).take(records.len()).collect())
    }

    async fn delete_older_than(&self, _retention_seconds: u64) -> Result<u64, StoreError> {
        // The memory stream keeps no receive times: every record is new.
        Ok(0)
    }
}

/// Privacy settings in memory, by account.
#[derive(Clone, Default)]
pub(crate) struct MemoryPrivacy(Arc<Mutex<HashMap<Uuid, Vec<PrivacySetting>>>>);

impl PrivacyStore for MemoryPrivacy {
    async fn settings(&self, account: Uuid) -> Result<Vec<PrivacySetting>, StoreError> {
        Ok(lock(&self.0).get(&account).cloned().unwrap_or_default())
    }

    async fn set(
        &self,
        account: Uuid,
        settings: &[(String, String)],
    ) -> Result<Vec<PrivacySetting>, StoreError> {
        let now_ms = unix_ms();
        let mut accounts = lock(&self.0);
        let stored = accounts.entry(account).or_default();
        let mut changed = Vec::with_capacity(settings.len());
        for (alias, value) in settings {
            let setting = PrivacySetting {
                alias: alias.clone(),
                value: value.clone(),
                updated_at_ms: now_ms,
            };
            stored.retain(|known| known.alias != *alias);
            stored.push(setting.clone());
            changed.push(setting);
        }
        Ok(changed)
    }
}

/// Contract checks shared by the in-memory and the real stores.
pub(crate) mod contract {
    use super::*;

    const HASH: &str = "$argon2id$stored";
    const FAR_FUTURE: i64 = 4_102_444_800;
    const PAST: i64 = 1_000_000_000;
    const SHORT_SECONDS: u64 = 60;
    const LONG_SECONDS: u64 = 600;
    const LIMIT_WINDOW: u64 = 60;
    const SESSION_EXPIRES_AT: i64 = 1_760_400_000;

    /// A name no other test run uses.
    pub(crate) fn unique(prefix: &str) -> String {
        format!("{prefix}{}", Uuid::new_v4().simple())
    }

    /// Checks an [`AccountStore`].
    pub(crate) async fn accounts(store: impl AccountStore) {
        let username = unique("u");
        let account = store
            .create_password_account(&username, HASH)
            .await
            .expect("create");
        assert!(matches!(
            store.create_password_account(&username, HASH).await,
            Err(StoreError::DuplicateUsername)
        ));
        assert_eq!(
            store.password_login(&username).await.expect("login"),
            Some((account.clone(), HASH.to_owned()))
        );
        assert_eq!(
            store.password_login(&unique("u")).await.expect("none"),
            None
        );

        let profile = DiscordProfile {
            user_id: unique("d"),
            username: "nelly".to_owned(),
            display_name: "Nelly".to_owned(),
            email: Some("nelly@example.org".to_owned()),
            email_verified: true,
        };
        let first = store.discord_account(&profile).await.expect("discord");
        let renamed = DiscordProfile {
            display_name: "Renamed".to_owned(),
            ..profile
        };
        let again = store.discord_account(&renamed).await.expect("again");
        assert_eq!(first, again);

        let live = crate::keys::token_hash(&unique("t"));
        let old = crate::keys::token_hash(&unique("t"));
        store
            .create_launcher_session(live, account.id, FAR_FUTURE)
            .await
            .expect("live");
        store
            .create_launcher_session(old, account.id, PAST)
            .await
            .expect("old");
        assert_eq!(
            store.launcher_session_account(live).await.expect("live"),
            Some(account)
        );
        assert_eq!(
            store.launcher_session_account(old).await.expect("old"),
            None
        );
        store.delete_launcher_session(live).await.expect("delete");
        assert_eq!(
            store.launcher_session_account(live).await.expect("gone"),
            None
        );
    }

    /// Checks a [`CharacterStore`] that knows the worlds `first` and `second`.
    pub(crate) async fn characters(store: impl CharacterStore, first: Uuid, second: Uuid) {
        let account = Uuid::new_v4();
        let name = unique("N");
        let new = |name: &str| NewCharacter {
            id: Uuid::new_v4(),
            account_id: account,
            name: name.to_owned(),
            appearance: serde_json::json!({"Name": name}),
        };
        assert!(
            store
                .characters(first, account)
                .await
                .expect("none")
                .is_empty()
        );
        assert!(!store.name_taken(&name).await.expect("free"));
        let created = store.create(first, &new(&name)).await.expect("create");
        assert_eq!(created.name, name);
        assert_eq!(
            store.characters(first, account).await.expect("list"),
            vec![created.clone()]
        );
        assert!(
            store
                .characters(second, account)
                .await
                .expect("other")
                .is_empty()
        );
        assert_eq!(
            store.character(first, created.id).await.expect("load"),
            Some(created.clone())
        );
        assert_eq!(
            store.character(second, created.id).await.expect("other"),
            None
        );
        assert!(store.name_taken(&name.to_uppercase()).await.expect("taken"));
        assert!(matches!(
            store.create(second, &new(&name.to_lowercase())).await,
            Err(StoreError::DuplicateCharacterName)
        ));
        assert!(matches!(
            store.create(Uuid::new_v4(), &new(&unique("N"))).await,
            Err(StoreError::UnknownWorld)
        ));
        assert!(matches!(
            store.characters(Uuid::new_v4(), account).await,
            Err(StoreError::UnknownWorld)
        ));
        assert!(
            !store
                .delete(first, Uuid::new_v4(), created.id)
                .await
                .expect("other account"),
            "another account cannot delete it"
        );
        assert!(
            store
                .delete(first, account, created.id)
                .await
                .expect("delete")
        );
        assert_eq!(
            store.character(first, created.id).await.expect("gone"),
            None
        );
        assert!(!store.name_taken(&name).await.expect("freed"));
        assert!(
            !store
                .delete(first, account, created.id)
                .await
                .expect("again")
        );
    }

    /// Checks a [`GameSessionStore`].
    pub(crate) async fn game_sessions(store: impl GameSessionStore) {
        let key = unique("k");
        let account = Uuid::new_v4();
        assert_eq!(store.resolve(&key, LONG_SECONDS).await.expect("none"), None);
        store
            .create(&key, account, SHORT_SECONDS)
            .await
            .expect("create");
        assert_eq!(
            store.resolve(&key, LONG_SECONDS).await.expect("resolve"),
            Some(account)
        );
        assert_eq!(
            store.resolve(&key, LONG_SECONDS).await.expect("again"),
            Some(account)
        );
        store.revoke(&key).await.expect("revoke");
        assert_eq!(store.resolve(&key, LONG_SECONDS).await.expect("gone"), None);
    }

    /// Checks a [`RateLimiter`].
    pub(crate) async fn rate_limiter(store: impl RateLimiter) {
        let key = unique("r");
        assert_eq!(store.increment(&key, LIMIT_WINDOW).await.expect("one"), 1);
        assert_eq!(store.increment(&key, LIMIT_WINDOW).await.expect("two"), 2);
        store.release(&key).await.expect("release");
        assert_eq!(store.increment(&key, LIMIT_WINDOW).await.expect("again"), 2);
        let absent = unique("r");
        store.release(&absent).await.expect("release absent");
        assert_eq!(
            store.increment(&absent, LIMIT_WINDOW).await.expect("first"),
            1
        );
    }

    /// Checks that a [`QueueStore`] can use its own clock: an admitted entry expires one entry
    /// lifetime after the current time.
    pub(crate) async fn queue_store_clock(store: impl QueueStore) {
        let settings = QueueSettings {
            world_id: Uuid::new_v4(),
            capacity: ONE_SLOT,
            stale_after_ms: STALE_AFTER_MS,
        };
        open_slots(&store, &settings).await;
        let before = unix_seconds();
        let status = store
            .update(
                &settings,
                Uuid::new_v4(),
                join(Uuid::new_v4()),
                QueueTime::StoreClock,
            )
            .await
            .expect("join");
        let Some(QueueStatus::Admitted(record)) = status else {
            panic!("expected an admitted entry, got {status:?}");
        };
        let lifetime = i64::try_from(ENTRY_TICKET_SECONDS).expect("small");
        assert!(
            (before + lifetime - CLOCK_SLACK_SECONDS
                ..=unix_seconds() + lifetime + CLOCK_SLACK_SECONDS)
                .contains(&record.expires_at),
            "expiry {} is not one entry lifetime after now",
            record.expires_at
        );
    }

    /// Free player slots of the queue checks that are not about free slots.
    const MANY_SLOTS: u32 = 1_000;
    /// Lifetime of the free-slot count in the checks.
    pub(crate) const SLOTS_TTL_MS: u64 = 60_000;
    const TWO_SLOTS: u32 = 2;
    const FIVE_SLOTS: u32 = 5;

    async fn open_slots(store: &impl QueueStore, settings: &QueueSettings) {
        store
            .set_free_slots(settings.world_id, MANY_SLOTS, SLOTS_TTL_MS)
            .await
            .expect("free slots");
    }

    /// Checks that a [`QueueStore`] admits only while the director reports free player slots,
    /// and never more than the entry ticket limit.
    pub(crate) async fn free_slots(store: impl QueueStore) {
        let settings = QueueSettings {
            world_id: Uuid::new_v4(),
            capacity: TWO_SLOTS,
            stale_after_ms: STALE_AFTER_MS,
        };
        let at = |seconds: u64| QueueTime::At(START_MS + seconds * SECOND_MS);
        let (first_ticket, _) = waiting(
            store
                .update(&settings, Uuid::new_v4(), join(Uuid::new_v4()), at(0))
                .await
                .expect("no slots"),
        );
        let slots = |count| store.set_free_slots(settings.world_id, count, SLOTS_TTL_MS);
        assert_eq!(
            store.free_slots(settings.world_id).await.expect("none"),
            0,
            "no report: no slot"
        );
        slots(ONE_SLOT).await.expect("one slot");
        assert_eq!(
            store.free_slots(settings.world_id).await.expect("read"),
            ONE_SLOT
        );
        let first = store
            .update(&settings, Uuid::new_v4(), refresh(&first_ticket), at(1))
            .await
            .expect("refresh");
        assert_eq!(first, None, "only the owner refreshes");
        let second = Uuid::new_v4();
        let (second_ticket, position) = waiting(
            store
                .update(&settings, second, join(Uuid::new_v4()), at(1))
                .await
                .expect("second"),
        );
        assert_eq!(position, 1, "the first entry took the one slot");
        slots(FIVE_SLOTS).await.expect("five slots");
        admitted(
            store
                .update(&settings, second, refresh(&second_ticket), at(2))
                .await
                .expect("second admitted"),
        );
        waiting(
            store
                .update(&settings, Uuid::new_v4(), join(Uuid::new_v4()), at(2))
                .await
                .expect("third"),
        );
    }

    /// Start time of the queue checks, ms.
    pub(crate) const START_MS: u64 = 1_760_400_000_000;
    /// Stale limit of the queue checks.
    pub(crate) const STALE_AFTER_MS: u64 = 40_000;
    /// Allowed difference between the test clock and the store clock.
    const CLOCK_SLACK_SECONDS: i64 = 2;
    const SECOND_MS: u64 = 1_000;
    const ONE_SLOT: u32 = 1;

    fn join(character_id: Uuid) -> QueueAction {
        QueueAction::Join {
            character_id,
            new_ticket_id: unique("t"),
        }
    }

    fn refresh(ticket_id: &str) -> QueueAction {
        QueueAction::Refresh {
            ticket_id: ticket_id.to_owned(),
        }
    }

    fn waiting(status: Option<QueueStatus>) -> (String, u64) {
        match status {
            Some(QueueStatus::Waiting {
                ticket_id,
                position,
            }) => (ticket_id, position),
            other => panic!("expected waiting, got {other:?}"),
        }
    }

    fn admitted(status: Option<QueueStatus>) -> EntryRecord {
        match status {
            Some(QueueStatus::Admitted(record)) => record,
            other => panic!("expected admitted, got {other:?}"),
        }
    }

    /// Checks a [`QueueStore`] with one slot of capacity. Times are seconds after [`START_MS`].
    pub(crate) async fn queue(store: impl QueueStore) {
        let settings = QueueSettings {
            world_id: Uuid::new_v4(),
            capacity: ONE_SLOT,
            stale_after_ms: STALE_AFTER_MS,
        };
        open_slots(&store, &settings).await;
        let at = |seconds: u64| QueueTime::At(START_MS + seconds * SECOND_MS);
        let (first, second, third) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let first_character = Uuid::new_v4();

        // 0 s: the first account takes the only slot.
        let record = admitted(
            store
                .update(&settings, first, join(first_character), at(0))
                .await
                .expect("first"),
        );
        assert_eq!(
            (record.account_id, record.character_id),
            (first, first_character)
        );
        assert_eq!(record.world_id, settings.world_id);

        // 1 s: the second account waits; joining again with the same character keeps the entry.
        let second_character = Uuid::new_v4();
        let (second_ticket, position) = waiting(
            store
                .update(&settings, second, join(second_character), at(1))
                .await
                .expect("second"),
        );
        assert_eq!(position, 1);
        let (again, _) = waiting(
            store
                .update(&settings, second, join(second_character), at(1))
                .await
                .expect("again"),
        );
        assert_eq!(again, second_ticket);

        // 2 s: the third account waits behind it; it cannot refresh the second ticket.
        let (third_ticket, position) = waiting(
            store
                .update(&settings, third, join(Uuid::new_v4()), at(2))
                .await
                .expect("third"),
        );
        assert_eq!(position, 2);
        assert_eq!(
            store
                .update(&settings, third, refresh(&second_ticket), at(2))
                .await
                .expect("stolen"),
            None
        );

        // 3 s: a new character replaces the second entry and goes to the back.
        let (replaced, position) = waiting(
            store
                .update(&settings, second, join(Uuid::new_v4()), at(3))
                .await
                .expect("replace"),
        );
        assert_ne!(replaced, second_ticket);
        assert_eq!(position, 2);
        assert_eq!(
            store
                .update(&settings, second, refresh(&second_ticket), at(3))
                .await
                .expect("old"),
            None
        );

        // 30 s: the third account refreshes and is first in line.
        let (_, position) = waiting(
            store
                .update(&settings, third, refresh(&third_ticket), at(30))
                .await
                .expect("refresh"),
        );
        assert_eq!(position, 1);

        // 61 s: the first record has expired, so the third account is admitted. The second
        // account stopped refreshing at 3 s and is removed as stale.
        let record = admitted(
            store
                .update(&settings, third, refresh(&third_ticket), at(61))
                .await
                .expect("admitted"),
        );
        assert_eq!(record.ticket_id, third_ticket);
        assert_eq!(
            store
                .update(&settings, second, refresh(&replaced), at(61))
                .await
                .expect("stale"),
            None
        );
    }

    /// Checks [`QueueStore::take_entry`]: a record is returned one time, and taking it frees
    /// its admission slot.
    pub(crate) async fn take_entry(store: impl QueueStore) {
        let settings = QueueSettings {
            world_id: Uuid::new_v4(),
            capacity: ONE_SLOT,
            stale_after_ms: STALE_AFTER_MS,
        };
        open_slots(&store, &settings).await;
        let at = |seconds: u64| QueueTime::At(START_MS + seconds * SECOND_MS);
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        let record = admitted(
            store
                .update(&settings, first, join(Uuid::new_v4()), at(0))
                .await
                .expect("first"),
        );
        let (second_ticket, _) = waiting(
            store
                .update(&settings, second, join(Uuid::new_v4()), at(1))
                .await
                .expect("second"),
        );
        assert_eq!(
            store.take_entry(&record.ticket_id).await.expect("take"),
            Some(record.clone())
        );
        assert_eq!(
            store.take_entry(&record.ticket_id).await.expect("again"),
            None
        );
        assert_eq!(store.take_entry(&unique("t")).await.expect("unknown"), None);
        let next = admitted(
            store
                .update(&settings, second, refresh(&second_ticket), at(2))
                .await
                .expect("slot freed"),
        );
        assert_eq!(next.ticket_id, second_ticket);
        assert_eq!(
            store
                .update(&settings, first, refresh(&record.ticket_id), at(2))
                .await
                .expect("taken ticket"),
            None
        );
    }

    /// Checks a [`DiscordStateStore`].
    pub(crate) async fn discord_states(store: impl DiscordStateStore) {
        let state = unique("s");
        assert_eq!(
            store.poll(&state).await.expect("unknown"),
            DiscordAttempt::Unknown
        );
        store
            .begin(&state, "verifier", LONG_SECONDS)
            .await
            .expect("begin");
        assert_eq!(
            store
                .take_verifier(&state)
                .await
                .expect("verifier")
                .as_deref(),
            Some("verifier")
        );
        assert_eq!(store.take_verifier(&state).await.expect("used"), None);
        assert_eq!(
            store.poll(&state).await.expect("pending"),
            DiscordAttempt::Pending
        );
        let session = LauncherSession {
            launcher_token: "token".to_owned(),
            expires_at: SESSION_EXPIRES_AT,
        };
        store
            .complete(&state, &session, LONG_SECONDS)
            .await
            .expect("complete");
        assert_eq!(
            store.poll(&state).await.expect("ready"),
            DiscordAttempt::Ready(session)
        );
        assert_eq!(
            store.poll(&state).await.expect("taken"),
            DiscordAttempt::Unknown
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_accounts_follow_the_contract() {
        contract::accounts(MemoryAccounts::default()).await;
    }

    #[tokio::test]
    async fn memory_characters_follow_the_contract() {
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        contract::characters(MemoryCharacters::new(&[first, second]), first, second).await;
    }

    #[tokio::test]
    async fn memory_game_sessions_follow_the_contract() {
        contract::game_sessions(MemoryGameSessions::default()).await;
    }

    #[tokio::test]
    async fn memory_rate_limiter_follows_the_contract() {
        contract::rate_limiter(MemoryRateLimiter::default()).await;
    }

    #[tokio::test]
    async fn memory_discord_states_follow_the_contract() {
        contract::discord_states(MemoryDiscordStates::default()).await;
    }

    #[tokio::test]
    async fn memory_queue_follows_the_contract() {
        contract::queue(MemoryQueue::default()).await;
        contract::queue_store_clock(MemoryQueue::default()).await;
        contract::take_entry(MemoryQueue::default()).await;
        contract::free_slots(MemoryQueue::default()).await;
    }

    const ALREADY_EXPIRED: u64 = 0;
    const TTL_SECONDS: u64 = 60;

    #[tokio::test]
    async fn expired_game_session_is_gone() {
        let store = MemoryGameSessions::default();
        store
            .create("k", Uuid::new_v4(), ALREADY_EXPIRED)
            .await
            .expect("create");
        assert_eq!(
            store.resolve("k", TTL_SECONDS).await.expect("resolve"),
            None
        );
    }
}
