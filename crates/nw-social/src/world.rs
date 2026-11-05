// SPDX-License-Identifier: AGPL-3.0-or-later
//! The state of the world's social life and its inputs: presence, the deliveries, and the
//! dispatch of the client calls to [`crate::chat`], [`crate::people`], [`crate::groups`] and
//! [`crate::companies`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use nw_bus::{
    BusId, LeaveReason, SocialDelivery, SocialFacetKind, SocialHeader, SocialInput, SocialPlayer,
};
use nw_db::{CoinReturn, Company, LinkKind, SocialGroup, SocialRecords, SocialSettings};
use nw_messages::FacetHeader;
use nw_messages::social::players::{ONLINE_STATE_OFFLINE, ONLINE_STATE_ONLINE};
use nw_messages::social::{ICON_LEN, PlayerPush, SocialCall, SocialFacet, SocialPush};
use uuid::Uuid;

use crate::companies::CompanyRules;
use crate::effects::{Effect, StoreOp};
use crate::privacy::PrivacyByAccount;
use crate::profanity::ProfanityFilter;
use crate::throttle::ChatThrottle;
use crate::views::{GroupVitals, SentViews, vitals_of};
use crate::voice::VoiceService;

/// Metric: social calls by `result`.
pub(crate) const METRIC_CALLS: &str = "social_calls_total";
/// Metric label of a call's result.
pub(crate) const RESULT_LABEL: &str = "result";
/// A call that was taken.
pub(crate) const TAKEN: &str = "taken";
/// A call whose body does not read.
const BAD: &str = "bad";
/// A call of a player the service does not know online.
const STRANGER: &str = "stranger";
/// A call the rules refuse.
pub(crate) const REFUSED: &str = "refused";
/// Metric: online players.
const METRIC_ONLINE: &str = "social_online_players";

/// The rules of the world's social service.
#[derive(Debug, Clone, PartialEq)]
pub struct SocialRules {
    /// The world (the world id text of the player data entries).
    pub world_id: Uuid,
    /// The voice service; `None` when the community runs none (voice calls stay unanswered).
    pub voice: Option<VoiceService>,
    /// The company rules.
    pub companies: CompanyRules,
}

/// What the service loaded at start.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Loaded {
    /// The stored state.
    pub records: SocialRecords,
    /// The icon of each character from its saved look, for characters never seen online.
    pub icons: HashMap<Uuid, [u8; ICON_LEN]>,
}

/// One character as the social state knows it.
#[derive(Debug, Clone)]
pub(crate) struct Person {
    pub(crate) name: String,
    /// Its account; `None` for a character the database did not list at start.
    pub(crate) account: Option<Uuid>,
    pub(crate) settings: SocialSettings,
    pub(crate) friends: BTreeSet<Uuid>,
    /// Friend invites this character sent.
    pub(crate) invites_out: BTreeSet<Uuid>,
    /// Friend invites this character got.
    pub(crate) invites_in: BTreeSet<Uuid>,
    pub(crate) blocks: BTreeSet<Uuid>,
    pub(crate) mutes: BTreeSet<Uuid>,
    /// Its sim's view of it while online.
    pub(crate) online: Option<SocialPlayer>,
    /// Its company and rank.
    pub(crate) company: Option<(Uuid, u8)>,
    /// Its group.
    pub(crate) group: Option<Uuid>,
    /// Its recent chat lines.
    pub(crate) throttle: ChatThrottle,
    /// What its group shows of its health.
    pub(crate) vitals: GroupVitals,
}

impl Person {
    fn new(name: String, account: Option<Uuid>, icon: Option<[u8; ICON_LEN]>) -> Self {
        Self {
            name,
            account,
            settings: SocialSettings {
                icon,
                ..SocialSettings::default()
            },
            friends: BTreeSet::new(),
            invites_out: BTreeSet::new(),
            invites_in: BTreeSet::new(),
            blocks: BTreeSet::new(),
            mutes: BTreeSet::new(),
            online: None,
            company: None,
            group: None,
            throttle: ChatThrottle::default(),
            vitals: GroupVitals::default(),
        }
    }
}

/// A company with its members in join order.
#[derive(Debug, Clone)]
pub(crate) struct CompanyState {
    pub(crate) record: Company,
    pub(crate) members: Vec<Uuid>,
    /// Invited characters and who invited them.
    pub(crate) invites: BTreeMap<Uuid, Uuid>,
}

/// An open group invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GroupInvite {
    pub(crate) inviter: Uuid,
    pub(crate) invitee: Uuid,
    /// Unix milliseconds when it was made.
    pub(crate) created_ms: u64,
}

/// The coins of one treasury transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CoinMove {
    pub(crate) character: Uuid,
    pub(crate) company: Uuid,
    /// Hundredths of a coin.
    pub(crate) amount: u64,
}

/// A treasury transfer between its steps ([`crate::treasury`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transfer {
    /// A deposit: its coins leave the player, then enter the treasury.
    Deposit(CoinMove),
    /// Deposit coins that go back to the player: the treasury did not take them.
    DepositReturn(CoinMove),
    /// A withdrawal: its coins leave the treasury, then reach the player.
    Withdraw(CoinMove),
    /// Withdrawn coins that the treasury takes back: the player did not get them.
    Refund(CoinMove),
    /// Coin owed to the player that goes to it now.
    Debt(CoinReturn),
}

/// The social state of one world.
#[derive(Debug)]
pub struct SocialWorld {
    pub(crate) rules: SocialRules,
    pub(crate) filter: ProfanityFilter,
    pub(crate) people: HashMap<Uuid, Person>,
    /// Characters by lowercase name.
    pub(crate) names: HashMap<String, Uuid>,
    pub(crate) companies: HashMap<Uuid, CompanyState>,
    /// Companies by lowercase name.
    pub(crate) company_names: HashMap<String, Uuid>,
    pub(crate) groups: HashMap<Uuid, SocialGroup>,
    pub(crate) group_invites: HashMap<[u8; 16], GroupInvite>,
    /// Done treasury withdrawals (and, from the database, their refunds) per character: Unix
    /// milliseconds and change.
    pub(crate) withdrawals: HashMap<Uuid, Vec<(u64, i64)>>,
    pub(crate) transfers: HashMap<u64, Transfer>,
    /// Coin owed to each character and not on its way to it now (world migration 0031).
    pub(crate) owed: HashMap<Uuid, Vec<CoinReturn>>,
    /// The phase of the instance each instance sim runs (its heartbeat); a sim without one is in
    /// the open world.
    pub(crate) sim_phases: HashMap<BusId, u64>,
    /// The privacy settings of the accounts that set one ([`crate::privacy`]).
    pub(crate) privacy: PrivacyByAccount,
    pub(crate) next_ticket: u64,
    pub(crate) out: Vec<Effect>,
    /// Characters whose replicated states this input may have changed ([`crate::views`]).
    pub(crate) touched: BTreeSet<Uuid>,
    /// The replicated states last sent to each online character.
    pub(crate) sent: HashMap<Uuid, SentViews>,
    /// Unix milliseconds of each company invite made since the start, by (company, invitee).
    pub(crate) company_invite_ms: HashMap<(Uuid, Uuid), u64>,
}

impl SocialWorld {
    /// The state of `loaded` under `rules`, filtering with `filter`.
    pub fn new(rules: SocialRules, filter: ProfanityFilter, loaded: Loaded) -> Self {
        let Loaded { records, icons } = loaded;
        let mut people: HashMap<Uuid, Person> = records
            .characters
            .iter()
            .map(|character| {
                (
                    character.id,
                    Person::new(
                        character.name.clone(),
                        Some(character.account),
                        icons.get(&character.id).copied(),
                    ),
                )
            })
            .collect();
        let names = records
            .characters
            .iter()
            .map(|character| (character.name.to_lowercase(), character.id))
            .collect();
        for (character, settings) in records.settings {
            if let Some(person) = people.get_mut(&character) {
                let icon = settings.icon.or(person.settings.icon);
                person.settings = SocialSettings { icon, ..settings };
            }
        }
        for (character, other, kind) in records.links {
            let Some(person) = people.get_mut(&character) else {
                continue;
            };
            match kind {
                LinkKind::Friend => {
                    person.friends.insert(other);
                }
                LinkKind::Invite => {
                    person.invites_out.insert(other);
                }
                LinkKind::Block => {
                    person.blocks.insert(other);
                }
                LinkKind::Mute => {
                    person.mutes.insert(other);
                }
            }
            if kind == LinkKind::Invite
                && let Some(invitee) = people.get_mut(&other)
            {
                invitee.invites_in.insert(character);
            }
        }
        let mut companies: HashMap<Uuid, CompanyState> = records
            .companies
            .into_iter()
            .map(|record| {
                (
                    record.id,
                    CompanyState {
                        record,
                        members: Vec::new(),
                        invites: BTreeMap::new(),
                    },
                )
            })
            .collect();
        for member in records.members {
            if let (Some(company), Some(person)) = (
                companies.get_mut(&member.company),
                people.get_mut(&member.character),
            ) {
                company.members.push(member.character);
                person.company = Some((member.company, member.rank));
            }
        }
        for invite in records.invites {
            if let Some(company) = companies.get_mut(&invite.company) {
                company.invites.insert(invite.character, invite.inviter);
            }
        }
        let company_names = companies
            .values()
            .map(|company| (company.record.name.to_lowercase(), company.record.id))
            .collect();
        let mut groups = HashMap::new();
        for group in records.groups {
            for member in &group.members {
                if let Some(person) = people.get_mut(member) {
                    person.group = Some(group.id);
                }
            }
            groups.insert(group.id, group);
        }
        let mut withdrawals: HashMap<Uuid, Vec<(u64, i64)>> = HashMap::new();
        for (character, at_ms, change) in records.withdrawals {
            withdrawals
                .entry(character)
                .or_default()
                .push((at_ms, change));
        }
        let mut owed: HashMap<Uuid, Vec<CoinReturn>> = HashMap::new();
        for debt in records.coin_returns {
            owed.entry(debt.character).or_default().push(debt);
        }
        Self {
            rules,
            filter,
            people,
            names,
            companies,
            company_names,
            groups,
            group_invites: HashMap::new(),
            withdrawals,
            transfers: HashMap::new(),
            owed,
            sim_phases: HashMap::new(),
            privacy: PrivacyByAccount::new(),
            next_ticket: 0,
            out: Vec::new(),
            touched: BTreeSet::new(),
            sent: HashMap::new(),
            company_invite_ms: HashMap::new(),
        }
    }

    /// Takes one input of sim `sim` at Unix milliseconds `now_ms`.
    pub fn input(&mut self, sim: &BusId, input: SocialInput, now_ms: u64) -> Vec<Effect> {
        match input {
            SocialInput::Joined(player) => {
                let character = player.character;
                self.player_here(player, now_ms);
                // The sim gave the player a new card: every state goes again.
                self.forget_views(character);
            }
            SocialInput::Updated(player) => self.player_here(player, now_ms),
            SocialInput::Vitals(vitals) => {
                if let Some(person) = self.people.get_mut(&vitals.character) {
                    person.vitals = vitals_of(&vitals);
                    self.touch(vitals.character);
                }
            }
            SocialInput::Left { character, reason } => {
                let here = self
                    .people
                    .get(&character)
                    .and_then(|person| person.online.as_ref())
                    .is_some_and(|online| online.sim == *sim);
                if here && reason == LeaveReason::Logout {
                    self.went_offline(character, now_ms);
                }
            }
            SocialInput::Call {
                character,
                type_index,
                body,
            } => self.call(character, type_index, &body, now_ms),
        }
        self.refresh_views(now_ms);
        std::mem::take(&mut self.out)
    }

    /// Sim `sim` stopped beating: its players are offline.
    pub fn sim_lost(&mut self, sim: &BusId, now_ms: u64) -> Vec<Effect> {
        self.sim_phases.remove(sim);
        let gone: Vec<Uuid> = self
            .people
            .iter()
            .filter(|(_, person)| {
                person
                    .online
                    .as_ref()
                    .is_some_and(|online| online.sim == *sim)
            })
            .map(|(character, _)| *character)
            .collect();
        for character in gone {
            self.went_offline(character, now_ms);
        }
        self.refresh_views(now_ms);
        std::mem::take(&mut self.out)
    }

    /// The online players.
    pub fn online_count(&self) -> usize {
        self.people
            .values()
            .filter(|person| person.online.is_some())
            .count()
    }

    fn player_here(&mut self, player: SocialPlayer, now_ms: u64) {
        let character = player.character;
        if !self.people.contains_key(&character) {
            self.names.insert(player.name.to_lowercase(), character);
            self.people.insert(
                character,
                Person::new(player.name.clone(), None, Some(player.icon)),
            );
            // Its account names its privacy settings.
            self.out.push(Effect::FindAccount { character });
        }
        let Some(person) = self.people.get_mut(&character) else {
            return;
        };
        let came_online = person.online.is_none();
        let look_changed = person.settings.level != player.level
            || person.settings.icon != Some(player.icon)
            || person.settings.faction != player.faction;
        person.settings.level = player.level;
        person.settings.icon = Some(player.icon);
        person.settings.faction = player.faction;
        if came_online {
            person.settings.last_online_ms = Some(now_ms);
        }
        person.online = Some(player);
        if came_online || look_changed {
            self.store_settings(character);
            self.touch(character);
        }
        if came_online {
            metrics::gauge!(METRIC_ONLINE).set(self.online_count() as f64);
            self.tell_watchers(character, ONLINE_STATE_ONLINE);
            self.pay_debts(character);
        }
    }

    /// The database holds `account` for `character` ([`Effect::FindAccount`]).
    pub fn account_found(&mut self, character: Uuid, account: Uuid) {
        if let Some(person) = self.people.get_mut(&character) {
            person.account = Some(account);
        }
    }

    /// Sim `sim` runs an instance of phase `phase` (`Some`), or none (its heartbeat): its players
    /// are in that phase.
    pub fn set_sim_phase(&mut self, sim: &BusId, phase: Option<u64>) {
        match phase {
            Some(phase) => {
                self.sim_phases.insert(sim.clone(), phase);
            }
            None => {
                self.sim_phases.remove(sim);
            }
        }
    }

    /// The phase `character` is in: its sim's instance, else the open world (also when offline).
    pub(crate) fn phase_of(&self, character: Uuid) -> Option<u64> {
        self.people
            .get(&character)
            .and_then(|person| person.online.as_ref())
            .and_then(|online| self.sim_phases.get(&online.sim))
            .copied()
    }

    fn went_offline(&mut self, character: Uuid, now_ms: u64) {
        let Some(person) = self.people.get_mut(&character) else {
            return;
        };
        if person.online.take().is_none() {
            return;
        }
        person.settings.last_online_ms = Some(now_ms);
        self.store_settings(character);
        // Offline: nothing more goes to it; its group and company see it go.
        self.sent.remove(&character);
        self.touch(character);
        self.tell_watchers(character, ONLINE_STATE_OFFLINE);
        self.drop_group_invites_of(character);
        metrics::gauge!(METRIC_ONLINE).set(self.online_count() as f64);
    }

    /// Tells the friends, company members and group members of `character` its online state.
    fn tell_watchers(&mut self, character: Uuid, state: u8) {
        let watchers: Vec<Uuid> = self
            .watchers(character)
            .into_iter()
            .filter(|watcher| self.sees_online(character, *watcher))
            .collect();
        self.push(
            &watchers,
            SocialPush::Player(PlayerPush::OnlineState {
                character: character.hyphenated().to_string(),
                state,
            }),
        );
    }

    /// The friends, company members and group members of `character`, without it.
    pub(crate) fn watchers(&self, character: Uuid) -> Vec<Uuid> {
        let Some(person) = self.people.get(&character) else {
            return Vec::new();
        };
        let mut watchers: BTreeSet<Uuid> = person.friends.clone();
        if let Some(company) = person
            .company
            .and_then(|(company, _)| self.companies.get(&company))
        {
            watchers.extend(company.members.iter().copied());
        }
        if let Some(group) = person.group.and_then(|group| self.groups.get(&group)) {
            watchers.extend(group.members.iter().copied());
        }
        watchers.remove(&character);
        watchers.into_iter().collect()
    }

    fn call(&mut self, character: Uuid, type_index: u64, body: &[u8], now_ms: u64) {
        let Some(decoded) = SocialCall::decode(type_index, body) else {
            return;
        };
        let (header, call) = match decoded {
            Ok(decoded) => decoded,
            Err(error) => {
                metrics::counter!(METRIC_CALLS, RESULT_LABEL => BAD).increment(1);
                tracing::warn!(type_index, %error, "social call does not read");
                return;
            }
        };
        if !self.is_online(character) {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => STRANGER).increment(1);
            return;
        }
        match call {
            SocialCall::Chat(call) => self.chat_call(character, call, now_ms),
            SocialCall::Player(call) => self.player_call(character, header, call),
            SocialCall::Group(call) => self.group_call(character, call, now_ms),
            SocialCall::Guild(call) => self.guild_call(character, header, call, now_ms),
            SocialCall::Voice(call) => self.voice_call(character, header, call, now_ms),
        }
    }

    pub(crate) fn is_online(&self, character: Uuid) -> bool {
        self.people
            .get(&character)
            .is_some_and(|person| person.online.is_some())
    }

    /// The online character named `name` (any letter case).
    pub(crate) fn online_named(&self, name: &str) -> Option<Uuid> {
        self.names
            .get(&name.to_lowercase())
            .copied()
            .filter(|character| self.is_online(*character))
    }

    /// The character of UUID text `text` that the world has.
    pub(crate) fn known(&self, text: &str) -> Option<Uuid> {
        Uuid::parse_str(text)
            .ok()
            .filter(|character| self.people.contains_key(character))
    }

    pub(crate) fn store(&mut self, op: StoreOp) {
        self.out.push(Effect::Store(op));
    }

    pub(crate) fn store_settings(&mut self, character: Uuid) {
        if let Some(person) = self.people.get(&character) {
            let settings = person.settings.clone();
            self.store(StoreOp::Settings {
                character,
                settings,
            });
        }
    }

    /// Sends `push` to the online characters of `recipients` (each once), with each one's own
    /// header of the push's component.
    pub(crate) fn push(&mut self, recipients: &[Uuid], push: SocialPush) {
        let facet = kind_of(push.facet());
        self.deliver(recipients, SocialHeader::Facet(facet), &push);
    }

    /// Sends `push` to `caller` as the answer of the call with header `header`.
    pub(crate) fn answer(&mut self, caller: Uuid, header: FacetHeader, push: SocialPush) {
        let header = SocialHeader::Echo {
            target: header.target,
            entity: header.entity,
        };
        self.deliver(&[caller], header, &push);
    }

    fn deliver(&mut self, recipients: &[Uuid], header: SocialHeader, push: &SocialPush) {
        let args = match push.encode_args() {
            Ok(args) => args,
            Err(error) => {
                tracing::warn!(type_index = push.type_index(), %error, "social push not encoded");
                return;
            }
        };
        let mut by_sim: BTreeMap<BusId, Vec<Uuid>> = BTreeMap::new();
        let unique: BTreeSet<Uuid> = recipients.iter().copied().collect();
        for character in unique {
            if let Some(online) = self
                .people
                .get(&character)
                .and_then(|person| person.online.as_ref())
            {
                by_sim
                    .entry(online.sim.clone())
                    .or_default()
                    .push(character);
            }
        }
        for (sim, recipients) in by_sim {
            self.out.push(Effect::Deliver {
                sim,
                delivery: SocialDelivery {
                    recipients,
                    header,
                    type_index: push.type_index(),
                    args: args.clone(),
                },
            });
        }
    }

    pub(crate) fn ticket(&mut self) -> u64 {
        self.next_ticket += 1;
        self.next_ticket
    }
}

/// The bus kind of a social component.
pub(crate) const fn kind_of(facet: SocialFacet) -> SocialFacetKind {
    match facet {
        SocialFacet::Chat => SocialFacetKind::Chat,
        SocialFacet::Social => SocialFacetKind::Social,
        SocialFacet::Groups => SocialFacetKind::Groups,
        SocialFacet::Guilds => SocialFacetKind::Guilds,
        SocialFacet::VoiceChat => SocialFacetKind::VoiceChat,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nw_messages::social::chat::{CHAT_MUTE_SET_TYPE_INDEX, SEND_CHAT_MESSAGE_TYPE_INDEX};

    pub(crate) const NOW_MS: u64 = 1_800_000_000_000;
    pub(crate) const HEADER: FacetHeader = FacetHeader {
        target: 11,
        entity: 22,
    };

    pub(crate) fn sim(name: &str) -> BusId {
        BusId::try_from(name.to_owned()).expect("id")
    }

    pub(crate) fn character(number: u128) -> Uuid {
        Uuid::from_u128(number)
    }

    pub(crate) fn player(number: u128, name: &str, on: &str) -> SocialPlayer {
        SocialPlayer {
            character: character(number),
            name: name.to_owned(),
            level: 10,
            icon: [1; ICON_LEN],
            sim: sim(on),
            level_name: "newworld_vitaeeterna".to_owned(),
            territories: vec![1],
            faction: 1,
            position: [100.0, 100.0],
        }
    }

    /// A world with characters 1 (Ann), 2 (Bob) and 3 (Cid), none online.
    pub(crate) fn world() -> SocialWorld {
        let characters = [(1, "Ann"), (2, "Bob"), (3, "Cid")]
            .into_iter()
            .map(|(number, name)| nw_db::SocialCharacter {
                id: character(number),
                account: Uuid::from_u128(number + 1000),
                name: name.to_owned(),
                appearance: serde_json::json!({}),
            })
            .collect();
        SocialWorld::new(
            SocialRules {
                world_id: Uuid::from_u128(77),
                voice: None,
                companies: crate::companies::tests::RULES,
            },
            ProfanityFilter::of_lists(["_bad_\n".as_bytes()]),
            Loaded {
                records: SocialRecords {
                    characters,
                    ..SocialRecords::default()
                },
                icons: HashMap::new(),
            },
        )
    }

    pub(crate) fn join(world: &mut SocialWorld, number: u128, name: &str, on: &str) -> Vec<Effect> {
        world.input(
            &sim(on),
            SocialInput::Joined(player(number, name, on)),
            NOW_MS,
        )
    }

    /// A call of character `number` (on sim `on`) with arguments `args`.
    pub(crate) fn call(
        world: &mut SocialWorld,
        number: u128,
        on: &str,
        type_index: u64,
        args: &[u8],
        now_ms: u64,
    ) -> Vec<Effect> {
        let mut body = Vec::new();
        body.extend_from_slice(&HEADER.target.to_be_bytes());
        body.extend_from_slice(&HEADER.entity.to_be_bytes());
        body.extend_from_slice(args);
        world.input(
            &sim(on),
            SocialInput::Call {
                character: character(number),
                type_index,
                body,
            },
            now_ms,
        )
    }

    /// The deliveries of `effects` of type `type_index`.
    pub(crate) fn deliveries(effects: &[Effect], type_index: u64) -> Vec<SocialDelivery> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Deliver { delivery, .. } if delivery.type_index == type_index => {
                    Some(delivery.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// The pushes of `effects` of type `type_index`, decoded, with their recipients.
    pub(crate) fn pushes(effects: &[Effect], type_index: u64) -> Vec<(Vec<Uuid>, SocialPush)> {
        deliveries(effects, type_index)
            .into_iter()
            .map(|delivery| {
                let push = SocialPush::decode_args(delivery.type_index, &delivery.args)
                    .expect("a social push")
                    .expect("decodes");
                (delivery.recipients, push)
            })
            .collect()
    }

    pub(crate) fn text(value: &str) -> Vec<u8> {
        let mut bytes = vec![u8::try_from(value.len()).expect("short")];
        bytes.extend_from_slice(value.as_bytes());
        bytes
    }

    #[test]
    fn friends_hear_when_a_player_comes_online_and_leaves() {
        let mut world = world();
        world
            .people
            .get_mut(&character(1))
            .expect("Ann")
            .friends
            .insert(character(2));
        world
            .people
            .get_mut(&character(2))
            .expect("Bob")
            .friends
            .insert(character(1));
        join(&mut world, 2, "Bob", "sim-b");
        let effects = join(&mut world, 1, "Ann", "sim-a");
        let notices = pushes(
            &effects,
            nw_messages::social::players::ONLINE_STATE_CHANGE_TYPE_INDEX,
        );
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].0, [character(2)]);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Store(StoreOp::Settings { .. })))
        );
        let handoff = world.input(
            &sim("sim-a"),
            SocialInput::Left {
                character: character(1),
                reason: LeaveReason::Handoff,
            },
            NOW_MS,
        );
        assert!(handoff.is_empty(), "a handoff is no logout");
        let gone = world.sim_lost(&sim("sim-a"), NOW_MS);
        let notices = pushes(
            &gone,
            nw_messages::social::players::ONLINE_STATE_CHANGE_TYPE_INDEX,
        );
        assert_eq!(
            notices[0].1,
            SocialPush::Player(PlayerPush::OnlineState {
                character: character(1).hyphenated().to_string(),
                state: ONLINE_STATE_OFFLINE,
            })
        );
        assert_eq!(world.online_count(), 1);
    }

    #[test]
    fn calls_of_strangers_and_bad_bodies_change_nothing() {
        let mut world = world();
        let effects = call(
            &mut world,
            1,
            "sim-a",
            SEND_CHAT_MESSAGE_TYPE_INDEX,
            &[0; 4],
            NOW_MS,
        );
        assert!(effects.is_empty());
        join(&mut world, 1, "Ann", "sim-a");
        let effects = call(
            &mut world,
            1,
            "sim-a",
            CHAT_MUTE_SET_TYPE_INDEX,
            &[],
            NOW_MS,
        );
        assert!(effects.is_empty(), "a push type is no call");
        let effects = call(
            &mut world,
            1,
            "sim-a",
            SEND_CHAT_MESSAGE_TYPE_INDEX,
            &[1],
            NOW_MS,
        );
        assert!(effects.is_empty(), "a short body");
    }
}
