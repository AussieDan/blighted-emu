// SPDX-License-Identifier: AGPL-3.0-or-later
//! Companies: creation, names, invites, members and ranks, the message of the day, the crest,
//! the siege hour, the company list, and the treasury.
//!
//! Observed (retail captures): a new company has its creator's faction, one member and siege hour
//! [`NEW_COMPANY_SIEGE_HOUR`]; the creator then gets [`JOIN_UPDATE_COPIES`] 5429 with its id, as
//! does a player whose invite answer joined it (3247, then the 5429s); a removed member gets 3598;
//! a change of the company gets one 5429 to its members; companies hold at most
//! [`COMPANY_MEMBER_LIMIT`] members; a treasury withdrawal gets 5200 with
//! `TREASURY_SUCCESS`, a deposit 5201.
//!
//! The rights of the four ranks are the client's `GuildPrivilegeId` privileges ([`Privilege`]);
//! which rank has which is a best guess ([`RANK_PRIVILEGES`]: no capture shows a lower rank
//! acting and the rank data is the company service's). The join wait, the least level and the
//! treasury maximum are [`CompanyRules`].
//!
//! The client shows its own company from the `GuildsComponentReplicatedState`, which
//! [`crate::views`] sends.

use nw_db::{Company, CompanyInvite, CompanyListing, CompanyMember};
use nw_messages::FacetHeader;
use nw_messages::guild_lookup::{OtherGuildData, SiegeWindow, UnorderedGuildsMsg};
use nw_messages::social::guilds::{ListOrder, RANK_FAILED_SELF};
use nw_messages::social::{
    CharacterRef, GuildCall, GuildPush, SocialPush, crest_bytes, crest_of_bytes,
};
use uuid::Uuid;

use crate::effects::{Effect, StoreOp};
use crate::world::{CompanyState, METRIC_CALLS, REFUSED, RESULT_LABEL, SocialWorld};

/// Rank 0: the governor.
pub(crate) const RANK_GOVERNOR: u8 = 0;
/// Rank 1: a consul.
pub(crate) const RANK_CONSUL: u8 = 1;
/// Rank 2: an officer.
pub(crate) const RANK_OFFICER: u8 = 2;
/// Rank 3: a settler, the rank of a new member.
pub(crate) const RANK_SETTLER: u8 = 3;
/// The most members of a company: the largest member count of 3,012 retail company entries.
pub const COMPANY_MEMBER_LIMIT: usize = 100;
/// The siege hour of a new company (`creating_a_company`: 14.0 in its first look-up answer).
pub const NEW_COMPANY_SIEGE_HOUR: f32 = 14.0;
/// The 5429 copies a creator or a joining player got (`creating_a_company`,
/// `ranks-rights-compagny`: three each time).
pub const JOIN_UPDATE_COPIES: usize = 3;
/// The war and territory counts of a company: this server has no wars or territory owners.
const NO_COUNT: u32 = 0;
/// The id of no company.
const NO_COMPANY: [u8; 16] = [0; 16];

/// A page request of the company list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListQuery {
    order: ListOrder,
    descending: bool,
    page_size: u16,
    /// The offset of the page, as this server's token; empty for the first page.
    token: String,
}

/// `GuildPrivilegeId`, the client's company privileges with their values (enum
/// `FUN_143f116a0`). A rank's privileges are a `GuildRankData` of the company service (`m_name`,
/// `m_securityLevel`, `m_privilegeIds`, reflection `FUN_145ca4720`), which no capture carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Privilege {
    MemberInvite = 0,
    MemberRemove = 1,
    MemberPromote = 2,
    MemberDemote = 3,
    ChatListen = 4,
    ChatSpeak = 5,
    OfficerChatListen = 6,
    OfficerChatSpeak = 7,
    MotdSet = 8,
    NoteEdit = 9,
    OfficerNoteSee = 10,
    OfficerNoteEdit = 11,
    StructureBuild = 12,
    StructureUse = 13,
    StructureMove = 14,
    StructureSeal = 15,
    StructureAssign = 16,
    StructureDemolish = 17,
    TreasuryDeposit = 18,
    TreasuryWithdraw = 19,
    StructureWithdraw = 20,
    StructureDeposit = 21,
    TerritoryDemolish = 22,
    DeclareWar = 23,
    FarmPlantSeed = 24,
    StructureCancel = 25,
    TreasurySetDailyLimit = 26,
    TerritoryProjectManagement = 27,
    TerritoryTaxation = 28,
    WarRaidManage = 29,
    WarRaidBuildDefensiveStructure = 30,
    TreasuryWithdrawUnlimited = 31,
}

/// The privileges of a settler (best guess: the four ranks' privileges are not in any
/// capture or client data; chat, use and the deposit).
const SETTLER_PRIVILEGES: &[Privilege] = &[
    Privilege::ChatListen,
    Privilege::ChatSpeak,
    Privilege::StructureUse,
    Privilege::TreasuryDeposit,
    Privilege::FarmPlantSeed,
];
/// The privileges an officer has above a settler (best guess): members in, members out,
/// officer chat, the officer notes and the treasury within the daily limit.
const OFFICER_PRIVILEGES: &[Privilege] = &[
    Privilege::MemberInvite,
    Privilege::MemberRemove,
    Privilege::OfficerChatListen,
    Privilege::OfficerChatSpeak,
    Privilege::OfficerNoteSee,
    Privilege::StructureBuild,
    Privilege::StructureWithdraw,
    Privilege::StructureDeposit,
    Privilege::TreasuryWithdraw,
];
/// The privileges a consul has above an officer (best guess): ranks, the message of the day,
/// notes, structures, territory and war, and the treasury without the daily limit. The daily
/// limit itself stays the governor's.
const CONSUL_PRIVILEGES: &[Privilege] = &[
    Privilege::MemberPromote,
    Privilege::MemberDemote,
    Privilege::MotdSet,
    Privilege::NoteEdit,
    Privilege::OfficerNoteEdit,
    Privilege::StructureMove,
    Privilege::StructureSeal,
    Privilege::StructureAssign,
    Privilege::StructureDemolish,
    Privilege::TerritoryDemolish,
    Privilege::DeclareWar,
    Privilege::StructureCancel,
    Privilege::TerritoryProjectManagement,
    Privilege::TerritoryTaxation,
    Privilege::WarRaidManage,
    Privilege::WarRaidBuildDefensiveStructure,
    Privilege::TreasuryWithdrawUnlimited,
];
/// The privileges only a governor has.
const GOVERNOR_PRIVILEGES: &[Privilege] = &[Privilege::TreasurySetDailyLimit];

/// The privilege bitmap of `privileges`.
const fn bits(privileges: &[Privilege]) -> u32 {
    let mut bits = 0;
    let mut index = 0;
    while index < privileges.len() {
        bits |= 1 << privileges[index] as u32;
        index += 1;
    }
    bits
}

/// The privilege bitmaps of ranks 0 (governor) to 3 (settler); each rank has those of the ranks
/// below it.
const RANK_PRIVILEGES: [u32; 4] = {
    let settler = bits(SETTLER_PRIVILEGES);
    let officer = settler | bits(OFFICER_PRIVILEGES);
    let consul = officer | bits(CONSUL_PRIVILEGES);
    let mut ranks = [0; 4];
    ranks[RANK_GOVERNOR as usize] = consul | bits(GOVERNOR_PRIVILEGES);
    ranks[RANK_CONSUL as usize] = consul;
    ranks[RANK_OFFICER as usize] = officer;
    ranks[RANK_SETTLER as usize] = settler;
    ranks
};

/// What a rank may do: one privilege, or the governor's changes of the company itself (its
/// name, crest and siege window have no privilege id in the client's enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Right {
    /// A client privilege.
    Has(Privilege),
    /// Rename, change the crest or the siege window.
    Lead,
}

/// Whether rank `rank` has right `right`.
pub(crate) const fn may(rank: u8, right: Right) -> bool {
    match right {
        Right::Has(privilege) => {
            rank as usize <= RANK_SETTLER as usize
                && RANK_PRIVILEGES[rank as usize] & 1 << privilege as u32 != 0
        }
        Right::Lead => rank == RANK_GOVERNOR,
    }
}

/// The company rules of the world: the client's `javelin.social.*` keys, which the retail remote
/// configuration of the captures does not set (their client defaults: 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompanyRules {
    /// Milliseconds after leaving a company before a player joins one again
    /// (`guild-rejoining-cooldown-seconds`, `guild-switching-cooldown-seconds`: this server keeps
    /// no former company, so one wait covers both).
    pub join_cooldown_ms: u64,
    /// The least zero-based level that joins a company (`guild-join-required-level`).
    pub join_required_level: u32,
    /// The most coin a treasury holds, hundredths of a coin (`DepositCappedByTreasuryMax`).
    pub treasury_max: u64,
}

/// The look-up entry of company `listing` (3610, 1410).
pub fn other_guild(listing: &CompanyListing) -> OtherGuildData {
    OtherGuildData {
        guild: *listing.id.as_bytes(),
        name: listing.name.as_bytes().to_vec(),
        master: listing
            .governor
            .map(|governor| governor.hyphenated().to_string().into_bytes())
            .unwrap_or_default(),
        faction: listing.faction,
        crest: crest_of_bytes(&listing.crest),
        members: listing.members,
        claims: NO_COUNT,
        wars: NO_COUNT,
        siege_window: SiegeWindow {
            start_hour: listing.siege_hour,
            // The empty text of 17,556 of 18,114 retail entries: this server has no region rule.
            rule: Vec::new(),
        },
    }
}

impl SocialWorld {
    pub(crate) fn guild_call(
        &mut self,
        character: Uuid,
        header: FacetHeader,
        call: GuildCall,
        now_ms: u64,
    ) {
        match call {
            GuildCall::NameAvailability { request, name } => {
                let available = self.company_name_free(&name, None);
                let push = GuildPush::NameAvailable {
                    request,
                    name,
                    available,
                };
                self.answer(character, header, SocialPush::Guild(push));
            }
            GuildCall::Create { name, crest } => {
                self.create_company(character, name, crest_bytes(&crest));
            }
            GuildCall::Refresh => {
                let guild = self
                    .company_of(character)
                    .map_or(NO_COMPANY, |(id, _)| *id.as_bytes());
                self.push(
                    &[character],
                    SocialPush::Guild(GuildPush::DataUpdated { guild }),
                );
            }
            GuildCall::Invite { character: invitee } => {
                self.invite_to_company(character, &invitee, now_ms);
            }
            GuildCall::RespondToInvite { guild, accept } => {
                self.respond_to_invite(character, Uuid::from_bytes(guild), accept, now_ms);
            }
            GuildCall::Leave => self.leave_company(character, now_ms),
            GuildCall::RemoveMember { character: member } => {
                self.remove_from_company(character, &member, now_ms);
            }
            GuildCall::SetRank {
                character: member,
                rank,
            } => self.set_rank(character, &member, rank),
            GuildCall::Deposit { amount } => self.deposit(character, amount),
            GuildCall::Withdraw { amount } => self.withdraw(character, amount, now_ms),
            GuildCall::SetWithdrawalLimit { limit } => {
                let right = Right::Has(Privilege::TreasurySetDailyLimit);
                self.edit_company(character, right, |company| {
                    company.withdrawal_limit = Some(limit);
                });
            }
            GuildCall::EditMotd { text } => {
                self.edit_company(character, Right::Has(Privilege::MotdSet), |company| {
                    company.motd = text;
                    company.motd_author = Some(character);
                    company.motd_at_ms = Some(now_ms);
                });
            }
            GuildCall::EditCrest { crest } => {
                let edited = self.edit_company(character, Right::Lead, |company| {
                    company.crest = crest_bytes(&crest);
                    company.crest_changes = company.crest_changes.saturating_add(1);
                });
                if edited {
                    self.push(&[character], SocialPush::Guild(GuildPush::CrestEdited));
                }
            }
            GuildCall::EditName { name } => {
                let own = self.company_of(character).map(|(id, _)| id);
                if self.company_name_free(&name, own) {
                    let old = own
                        .and_then(|id| self.companies.get(&id))
                        .map(|company| company.record.name.to_lowercase());
                    let renamed = self.edit_company(character, Right::Lead, |company| {
                        company.name = name.clone();
                        company.renames = company.renames.saturating_add(1);
                    });
                    if renamed && let (Some(old), Some(id)) = (old, own) {
                        self.company_names.remove(&old);
                        self.company_names.insert(name.to_lowercase(), id);
                    }
                }
            }
            GuildCall::SetSiegeWindow { hour } if hour.is_finite() => {
                self.edit_company(character, Right::Lead, |company| company.siege_hour = hour);
            }
            GuildCall::SetSiegeWindow { .. } => {
                metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            }
            GuildCall::List {
                order,
                request,
                descending,
                page_size,
                token,
            } => {
                let query = ListQuery {
                    order,
                    descending,
                    page_size,
                    token,
                };
                self.list_companies(character, header, request, &query);
            }
        }
    }

    /// The company and rank of `character`.
    pub(crate) fn company_of(&self, character: Uuid) -> Option<(Uuid, u8)> {
        self.people
            .get(&character)
            .and_then(|person| person.company)
    }

    /// Whether `name` is a free, clean company name; `own` keeps its own name free.
    fn company_name_free(&self, name: &str, own: Option<Uuid>) -> bool {
        let name = name.trim();
        !name.is_empty()
            && self.filter.filter(name).is_none()
            && self
                .company_names
                .get(&name.to_lowercase())
                .is_none_or(|holder| Some(*holder) == own)
    }

    /// The listing of company `id` as a look-up shows it.
    fn listing(&self, id: Uuid) -> Option<CompanyListing> {
        let company = self.companies.get(&id)?;
        let governor = company.members.iter().copied().find(|member| {
            self.people
                .get(member)
                .and_then(|person| person.company)
                .is_some_and(|(_, rank)| rank == RANK_GOVERNOR)
        });
        Some(CompanyListing {
            id,
            name: company.record.name.clone(),
            governor,
            faction: company.record.faction,
            crest: company.record.crest,
            members: u32::try_from(company.members.len()).unwrap_or(u32::MAX),
            siege_hour: company.record.siege_hour,
        })
    }

    fn tell_company_changed(&mut self, company: Uuid, copies: usize, to: &[Uuid]) {
        for _ in 0..copies {
            self.push(
                to,
                SocialPush::Guild(GuildPush::DataUpdated {
                    guild: *company.as_bytes(),
                }),
            );
        }
    }

    fn members_of(&self, company: Uuid) -> Vec<Uuid> {
        self.companies
            .get(&company)
            .map(|company| company.members.clone())
            .unwrap_or_default()
    }

    fn create_company(&mut self, founder: Uuid, name: String, crest: [u8; nw_db::CREST_LEN]) {
        if self.company_of(founder).is_some() || !self.company_name_free(&name, None) {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        }
        let faction = self
            .people
            .get(&founder)
            .map_or(0, |person| person.settings.faction);
        let company = Company {
            id: Uuid::new_v4(),
            name: name.trim().to_owned(),
            faction,
            crest,
            motd: String::new(),
            motd_author: None,
            motd_at_ms: None,
            siege_hour: NEW_COMPANY_SIEGE_HOUR,
            treasury: 0,
            withdrawal_limit: None,
            renames: 0,
            crest_changes: 0,
        };
        let id = company.id;
        self.company_names.insert(company.name.to_lowercase(), id);
        self.companies.insert(
            id,
            CompanyState {
                record: company.clone(),
                members: vec![founder],
                invites: std::collections::BTreeMap::new(),
            },
        );
        if let Some(person) = self.people.get_mut(&founder) {
            person.company = Some((id, RANK_GOVERNOR));
        }
        self.store(StoreOp::CreateCompany { company, founder });
        self.tell_company_changed(id, JOIN_UPDATE_COPIES, &[founder]);
    }

    fn invite_to_company(&mut self, inviter: Uuid, invitee: &str, now_ms: u64) {
        let Some((company, rank)) = self.company_of(inviter) else {
            return;
        };
        let target = self
            .known(invitee)
            .filter(|target| *target != inviter && self.company_of(*target).is_none());
        let (Some(target), true) = (target, may(rank, Right::Has(Privilege::MemberInvite))) else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        if let Some(state) = self.companies.get_mut(&company) {
            state.invites.insert(target, inviter);
        }
        self.company_invite_ms.insert((company, target), now_ms);
        self.store(StoreOp::AddInvite(CompanyInvite {
            company,
            character: target,
            inviter,
        }));
    }

    fn respond_to_invite(&mut self, character: Uuid, company: Uuid, accept: bool, now_ms: u64) {
        let Some(state) = self.companies.get_mut(&company) else {
            return;
        };
        if state.invites.remove(&character).is_none() {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        }
        self.company_invite_ms.remove(&(company, character));
        let Some(state) = self.companies.get(&company) else {
            return;
        };
        let faction = state.record.faction;
        let room = state.members.len() < COMPANY_MEMBER_LIMIT;
        let rules = self.rules.companies;
        let (player_faction, level, left_at_ms) =
            self.people.get(&character).map_or((0, 0, None), |person| {
                (
                    person.settings.faction,
                    person.settings.level,
                    person.settings.company_left_at_ms,
                )
            });
        let cooled =
            left_at_ms.is_none_or(|left| now_ms.saturating_sub(left) >= rules.join_cooldown_ms);
        let joins = accept
            && room
            && cooled
            && level >= rules.join_required_level
            && self.company_of(character).is_none()
            && (faction == 0 || faction == player_faction);
        self.push(
            &[character],
            SocialPush::Guild(GuildPush::InviteResponded { accepted: joins }),
        );
        if !joins {
            self.store(StoreOp::RemoveInvite { company, character });
            return;
        }
        if let Some(state) = self.companies.get_mut(&company) {
            state.members.push(character);
        }
        if let Some(person) = self.people.get_mut(&character) {
            person.company = Some((company, RANK_SETTLER));
        }
        self.store(StoreOp::SetMember(CompanyMember {
            character,
            company,
            rank: RANK_SETTLER,
        }));
        self.tell_company_changed(company, JOIN_UPDATE_COPIES, &[character]);
        let others: Vec<Uuid> = self
            .members_of(company)
            .into_iter()
            .filter(|member| *member != character)
            .collect();
        self.tell_company_changed(company, 1, &others);
    }

    /// Takes `character` out of its company at `now_ms`; the company ends with its last member.
    fn take_out(&mut self, character: Uuid, company: Uuid, now_ms: u64) {
        if let Some(person) = self.people.get_mut(&character) {
            person.company = None;
            person.settings.company_left_at_ms = Some(now_ms);
        }
        self.store(StoreOp::RemoveMember {
            character,
            left_at_ms: now_ms,
        });
        let empty = self.companies.get_mut(&company).is_some_and(|state| {
            state.members.retain(|member| *member != character);
            state.members.is_empty()
        });
        if empty {
            if let Some(state) = self.companies.remove(&company) {
                self.company_names.remove(&state.record.name.to_lowercase());
            }
            self.store(StoreOp::DeleteCompany(company));
        } else {
            let members = self.members_of(company);
            self.tell_company_changed(company, 1, &members);
        }
    }

    /// The database refused a change of company `id` (its name is another company's there), and
    /// holds `stored` for it: the state takes the stored company, or ends a company the database
    /// does not hold, so memory and database agree. Its members are told.
    pub fn company_from_database(
        &mut self,
        id: Uuid,
        stored: Option<Company>,
        now_ms: u64,
    ) -> Vec<Effect> {
        let members = self.members_of(id);
        if let Some(state) = self.companies.get(&id) {
            self.company_names.remove(&state.record.name.to_lowercase());
        }
        match stored {
            Some(record) => {
                self.company_names.insert(record.name.to_lowercase(), id);
                if let Some(state) = self.companies.get_mut(&id) {
                    state.record = record;
                }
                self.tell_company_changed(id, 1, &members);
            }
            None => {
                tracing::error!(company = %id, "company not in the database; it ends");
                self.companies.remove(&id);
                for member in &members {
                    if let Some(person) = self.people.get_mut(member) {
                        person.company = None;
                    }
                }
                self.push(
                    &members,
                    SocialPush::Guild(GuildPush::DataUpdated { guild: NO_COMPANY }),
                );
            }
        }
        for member in members {
            self.touch(member);
        }
        self.refresh_views(now_ms);
        std::mem::take(&mut self.out)
    }

    fn leave_company(&mut self, character: Uuid, now_ms: u64) {
        let Some((company, rank)) = self.company_of(character) else {
            return;
        };
        let others = self.members_of(company).len() > 1;
        if rank == RANK_GOVERNOR && others {
            // `GuildMasterCantAcceptInvite`: a governor names another governor first.
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        }
        self.take_out(character, company, now_ms);
        self.push(
            &[character],
            SocialPush::Guild(GuildPush::DataUpdated { guild: NO_COMPANY }),
        );
    }

    fn remove_from_company(&mut self, actor: Uuid, member: &str, now_ms: u64) {
        let Some((company, rank)) = self.company_of(actor) else {
            return;
        };
        let target = self.known(member).and_then(|target| {
            self.company_of(target)
                .filter(|(theirs, their_rank)| *theirs == company && *their_rank > rank)
                .map(|_| target)
        });
        let (Some(target), true) = (target, may(rank, Right::Has(Privilege::MemberRemove))) else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        self.take_out(target, company, now_ms);
        self.push(&[target], SocialPush::Guild(GuildPush::Kicked));
    }

    fn set_rank(&mut self, actor: Uuid, member: &str, rank: u8) {
        let Some((company, actor_rank)) = self.company_of(actor) else {
            return;
        };
        let Some(target) = self.known(member) else {
            return;
        };
        if target == actor {
            let push = GuildPush::RankFailed {
                character: CharacterRef(Some(*target.as_bytes())),
                reason: RANK_FAILED_SELF,
            };
            self.push(&[actor], SocialPush::Guild(push));
            return;
        }
        let target_rank = self
            .company_of(target)
            .filter(|(theirs, _)| *theirs == company)
            .map(|(_, rank)| rank);
        let handover = actor_rank == RANK_GOVERNOR && rank == RANK_GOVERNOR;
        // A lower rank number is a higher rank: a change to a lower number is a promotion.
        let privilege = if target_rank.is_some_and(|their| rank < their) {
            Privilege::MemberPromote
        } else {
            Privilege::MemberDemote
        };
        let allowed = target_rank.is_some_and(|their| their > actor_rank)
            && rank <= RANK_SETTLER
            && may(actor_rank, Right::Has(privilege))
            && (rank > actor_rank || handover);
        if !allowed {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        }
        let mut changes = vec![(target, rank)];
        if handover {
            changes.push((actor, RANK_CONSUL));
        }
        for (character, rank) in changes {
            if let Some(person) = self.people.get_mut(&character) {
                person.company = Some((company, rank));
            }
            self.store(StoreOp::SetMember(CompanyMember {
                character,
                company,
                rank,
            }));
        }
        let members = self.members_of(company);
        self.tell_company_changed(company, 1, &members);
    }

    /// Applies `edit` to the company of `character` when its rank has `right`, stores it and
    /// tells the members. Returns whether it did.
    fn edit_company(
        &mut self,
        character: Uuid,
        right: Right,
        edit: impl FnOnce(&mut Company),
    ) -> bool {
        let Some((company, _)) = self
            .company_of(character)
            .filter(|(_, rank)| may(*rank, right))
        else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return false;
        };
        let Some(state) = self.companies.get_mut(&company) else {
            return false;
        };
        edit(&mut state.record);
        let record = state.record.clone();
        self.store(StoreOp::UpdateCompany(record));
        let members = self.members_of(company);
        self.tell_company_changed(company, 1, &members);
        true
    }

    fn list_companies(
        &mut self,
        caller: Uuid,
        header: FacetHeader,
        request: [u8; 16],
        query: &ListQuery,
    ) {
        let (order, descending, page_size) = (query.order, query.descending, query.page_size);
        let start: usize = query.token.parse().unwrap_or(0);
        let mut listings: Vec<CompanyListing> = self
            .companies
            .keys()
            .filter_map(|id| self.listing(*id))
            .collect();
        listings.sort_by(|a, b| {
            let by_name = a.name.to_lowercase().cmp(&b.name.to_lowercase());
            let ordered = match order {
                ListOrder::Name => by_name,
                ListOrder::Members => a.members.cmp(&b.members).then(by_name),
                // No company owns territory here: every claim count is 0.
                ListOrder::Claims => by_name,
            };
            if descending {
                ordered.reverse()
            } else {
                ordered
            }
        });
        let page: Vec<OtherGuildData> = listings
            .iter()
            .skip(start)
            .take(usize::from(page_size))
            .map(other_guild)
            .collect();
        let next = start + page.len();
        let token = if next < listings.len() {
            next.to_string()
        } else {
            String::new()
        };
        let push = GuildPush::OrderedGuilds {
            listing: UnorderedGuildsMsg {
                header,
                request,
                guilds: page,
            },
            token,
        };
        self.answer(caller, header, SocialPush::Guild(push));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use nw_messages::social::guilds::{
        CREATE_GUILD_TYPE_INDEX, CREATE_INVITE_TYPE_INDEX, GUILD_DATA_UPDATED_TYPE_INDEX,
        INVITE_RESPONDED_TYPE_INDEX, KICKED_TYPE_INDEX, LEAVE_GUILD_TYPE_INDEX,
        LIST_BY_NAME_TYPE_INDEX, NAME_AVAILABILITY_TYPE_INDEX, NAME_AVAILABLE_TYPE_INDEX,
        ORDERED_GUILDS_TYPE_INDEX, REMOVE_MEMBER_TYPE_INDEX, RESPOND_TO_INVITE_TYPE_INDEX,
        SET_MEMBER_RANK_TYPE_INDEX,
    };

    use super::*;
    use crate::world::tests::{NOW_MS, call, character, deliveries, join, pushes, text, world};

    const CREST: [u8; nw_db::CREST_LEN] = [0; nw_db::CREST_LEN];
    /// The rules of the test worlds: no join wait, no least level, a large treasury.
    pub(crate) const RULES: CompanyRules = CompanyRules {
        join_cooldown_ms: 0,
        join_required_level: 0,
        treasury_max: 1_000_000,
    };

    #[test]
    fn rank_privileges_grow_with_the_rank() {
        for privilege in [Privilege::ChatSpeak, Privilege::TreasuryDeposit] {
            assert!(may(RANK_SETTLER, Right::Has(privilege)), "{privilege:?}");
        }
        assert!(!may(RANK_SETTLER, Right::Has(Privilege::MemberInvite)));
        assert!(may(RANK_OFFICER, Right::Has(Privilege::TreasuryWithdraw)));
        assert!(!may(
            RANK_OFFICER,
            Right::Has(Privilege::TreasuryWithdrawUnlimited)
        ));
        assert!(!may(RANK_OFFICER, Right::Has(Privilege::MotdSet)));
        assert!(may(RANK_CONSUL, Right::Has(Privilege::MemberPromote)));
        assert!(!may(
            RANK_CONSUL,
            Right::Has(Privilege::TreasurySetDailyLimit)
        ));
        assert!(!may(RANK_CONSUL, Right::Lead));
        assert_eq!(RANK_PRIVILEGES[usize::from(RANK_GOVERNOR)], u32::MAX);
        assert!(may(RANK_GOVERNOR, Right::Lead));
        assert!(!may(RANK_SETTLER + 1, Right::Has(Privilege::ChatListen)));
    }

    #[test]
    fn a_join_waits_out_the_cooldown_and_needs_the_level() {
        const COOLDOWN_MS: u64 = 60_000;
        // One above the test players' level (10).
        const LEVEL: u32 = 11;
        let mut world = world();
        world.rules.companies = CompanyRules {
            join_cooldown_ms: COOLDOWN_MS,
            join_required_level: LEVEL,
            ..RULES
        };
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-a");
        create(&mut world, 1, "Alpha");
        let company = world.people[&character(1)].company.expect("company").0;
        let ask = |world: &mut SocialWorld| {
            call(
                world,
                1,
                "sim-a",
                CREATE_INVITE_TYPE_INDEX,
                &id_text(2),
                NOW_MS,
            );
            let mut answer = company.as_bytes().to_vec();
            answer.push(1);
            call(
                world,
                2,
                "sim-a",
                RESPOND_TO_INVITE_TYPE_INDEX,
                &answer,
                NOW_MS,
            );
            world.people[&character(2)].company.is_some()
        };
        assert!(!ask(&mut world), "below the level");
        let bob = world.people.get_mut(&character(2)).expect("Bob");
        bob.settings.level = LEVEL;
        bob.settings.company_left_at_ms = Some(NOW_MS - COOLDOWN_MS / 2);
        assert!(!ask(&mut world), "within the wait");
        world
            .people
            .get_mut(&character(2))
            .expect("Bob")
            .settings
            .company_left_at_ms = Some(NOW_MS - COOLDOWN_MS);
        assert!(ask(&mut world), "after the wait");
    }

    fn create(world: &mut SocialWorld, number: u128, name: &str) -> Vec<Effect> {
        let mut args = text(name);
        args.extend(CREST);
        call(
            world,
            number,
            "sim-a",
            CREATE_GUILD_TYPE_INDEX,
            &args,
            NOW_MS,
        )
    }

    fn id_text(number: u128) -> Vec<u8> {
        text(&character(number).hyphenated().to_string())
    }

    /// Ann creates Alpha and Bob joins it by invite.
    pub(crate) fn company_of_two() -> (SocialWorld, Uuid) {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-a");
        let effects = create(&mut world, 1, "Alpha");
        assert_eq!(
            deliveries(&effects, GUILD_DATA_UPDATED_TYPE_INDEX).len(),
            JOIN_UPDATE_COPIES
        );
        let company = world.people[&character(1)]
            .company
            .expect("Ann's company")
            .0;
        call(
            &mut world,
            1,
            "sim-a",
            CREATE_INVITE_TYPE_INDEX,
            &id_text(2),
            NOW_MS,
        );
        let mut answer = company.as_bytes().to_vec();
        answer.push(1);
        let effects = call(
            &mut world,
            2,
            "sim-a",
            RESPOND_TO_INVITE_TYPE_INDEX,
            &answer,
            NOW_MS,
        );
        assert_eq!(
            pushes(&effects, INVITE_RESPONDED_TYPE_INDEX)[0].1,
            SocialPush::Guild(GuildPush::InviteResponded { accepted: true })
        );
        assert_eq!(
            world.people[&character(2)].company,
            Some((company, RANK_SETTLER))
        );
        (world, company)
    }

    #[test]
    fn a_taken_or_listed_name_is_not_available() {
        let (mut world, _) = company_of_two();
        let ask = |world: &mut SocialWorld, name: &str| {
            let mut args = [7; 16].to_vec();
            args.extend(text(name));
            let effects = call(
                world,
                3,
                "sim-a",
                NAME_AVAILABILITY_TYPE_INDEX,
                &args,
                NOW_MS,
            );
            match pushes(&effects, NAME_AVAILABLE_TYPE_INDEX).pop() {
                Some((_, SocialPush::Guild(GuildPush::NameAvailable { available, .. }))) => {
                    available
                }
                other => panic!("an answer: {other:?}"),
            }
        };
        join(&mut world, 3, "Cid", "sim-a");
        assert!(!ask(&mut world, "ALPHA"));
        assert!(!ask(&mut world, "bad"));
        assert!(ask(&mut world, "Beta"));
        assert!(create(&mut world, 3, "alpha").is_empty(), "a taken name");
    }

    #[test]
    fn a_company_the_database_refused_follows_the_database() {
        let (mut world, company) = company_of_two();
        let mut stored = world.companies[&company].record.clone();
        world
            .companies
            .get_mut(&company)
            .expect("company")
            .record
            .name = "Renamed".to_owned();
        world.company_names.remove("alpha");
        world.company_names.insert("renamed".to_owned(), company);
        stored.motd = "stored".to_owned();
        let effects = world.company_from_database(company, Some(stored), NOW_MS);
        assert_eq!(world.companies[&company].record.name, "Alpha");
        assert_eq!(world.companies[&company].record.motd, "stored");
        assert_eq!(world.company_names.get("alpha"), Some(&company));
        assert!(!world.company_names.contains_key("renamed"));
        assert_eq!(deliveries(&effects, GUILD_DATA_UPDATED_TYPE_INDEX).len(), 1);
        let effects = world.company_from_database(company, None, NOW_MS);
        assert!(world.companies.is_empty() && world.company_names.is_empty());
        assert_eq!(world.people[&character(1)].company, None);
        assert_eq!(world.people[&character(2)].company, None);
        assert_eq!(
            pushes(&effects, GUILD_DATA_UPDATED_TYPE_INDEX)[0],
            (
                vec![character(1), character(2)],
                SocialPush::Guild(GuildPush::DataUpdated { guild: NO_COMPANY })
            ),
            "no memory-only company is left"
        );
    }

    #[test]
    fn ranks_removal_and_leaving_follow_the_rights() {
        let (mut world, company) = company_of_two();
        let mut rank = id_text(1);
        rank.push(RANK_OFFICER);
        let effects = call(
            &mut world,
            1,
            "sim-a",
            SET_MEMBER_RANK_TYPE_INDEX,
            &rank,
            NOW_MS,
        );
        assert_eq!(
            pushes(
                &effects,
                nw_messages::social::guilds::RANK_FAILED_TYPE_INDEX
            )
            .len(),
            1,
            "an own rank change fails as retail"
        );
        let mut rank = id_text(2);
        rank.push(RANK_GOVERNOR);
        call(
            &mut world,
            1,
            "sim-a",
            SET_MEMBER_RANK_TYPE_INDEX,
            &rank,
            NOW_MS,
        );
        assert_eq!(
            world.people[&character(2)].company,
            Some((company, RANK_GOVERNOR))
        );
        assert_eq!(
            world.people[&character(1)].company,
            Some((company, RANK_CONSUL))
        );
        let effects = call(
            &mut world,
            2,
            "sim-a",
            REMOVE_MEMBER_TYPE_INDEX,
            &id_text(1),
            NOW_MS,
        );
        assert_eq!(pushes(&effects, KICKED_TYPE_INDEX)[0].0, [character(1)]);
        let effects = call(&mut world, 2, "sim-a", LEAVE_GUILD_TYPE_INDEX, &[], NOW_MS);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Store(StoreOp::DeleteCompany(_))))
        );
        assert!(world.companies.is_empty() && world.company_names.is_empty());
    }

    #[test]
    fn the_list_pages_by_the_token() {
        let (mut world, _) = company_of_two();
        join(&mut world, 3, "Cid", "sim-a");
        create(&mut world, 3, "Beta");
        let page = |world: &mut SocialWorld, size: u16, token: &str| {
            let mut args = [8; 16].to_vec();
            args.push(0);
            args.extend(size.to_be_bytes());
            args.extend(text(token));
            let effects = call(world, 3, "sim-a", LIST_BY_NAME_TYPE_INDEX, &args, NOW_MS);
            let delivery = deliveries(&effects, ORDERED_GUILDS_TYPE_INDEX)
                .pop()
                .expect("a page");
            delivery.args
        };
        let first = page(&mut world, 1, "");
        assert!(first.ends_with(&text("1")), "the next page's token");
        let second = page(&mut world, 1, "1");
        assert!(second.ends_with(&text("")), "the last page");
    }
}
