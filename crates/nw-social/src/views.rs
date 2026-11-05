// SPDX-License-Identifier: AGPL-3.0-or-later
//! The replicated social states of each online player (run 15 social2 study): the groups state
//! (its group and the group invites it got or sent), the social state (friends, friend invites,
//! blocks), the company state (its company, members, ranks and the company invites it got),
//! the chat state (its mutes) and its group entity (the members' cards, health, mana, flags,
//! faction and level).
//!
//! The client shows the group panel, the friend list, the own company panel and the invite
//! popups from these states alone (`nw_messages::social::states`, `group_data`). After every
//! input the service works out the states of the players it touched and of their group and
//! company, and sends each state that changed since the last one it sent the player; a player
//! that arrives on a sim (a login or a handoff: the sim sent it a new card) gets them all again.
//! Lists go whole, each with a version above the last one sent in this stay.

use std::collections::BTreeSet;

use nw_bus::{GroupEntityDelivery, GroupEntityView, OwnStateDelivery, SocialStateKind};
use nw_messages::FullList;
use nw_messages::items::FIRST_VERSION;
use nw_messages::social::group_data::{GroupDataState, GroupMemberView};
use nw_messages::social::states::{
    ChatState, GUILD_INVITE_KIND, GroupInviteData, GroupsState, GuildInviteData, GuildsState,
    NO_GUILD_RANK, SocialState,
};
use nw_messages::social::{CharacterRef, crest_of_bytes, icon_of_bytes};
use nw_messages::wire::{MessageError, Writer};
use uuid::Uuid;

use crate::effects::{Effect, StoreOp};
use crate::world::SocialWorld;

/// Nanoseconds per millisecond: the states carry Unix nanoseconds.
const NANOS_PER_MS: u64 = 1_000_000;
/// A member's online status in the company lists.
const ONLINE: u8 = 1;
/// A member's offline status in the company lists.
const OFFLINE: u8 = 0;
/// The last-online time of a member that is online (retail: 0 for the online members).
const ONLINE_LAST_SEEN: u64 = 0;
/// The id of no group or company.
const NO_ID: [u8; 16] = [0; 16];
/// The platform id text of an inviter: this server has no platform ids.
const NO_PLATFORM_ID: &str = "";

/// What the group of a player shows of it: health and mana in percent, and its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GroupVitals {
    pub(crate) health_percent: u8,
    pub(crate) mana_percent: u8,
    pub(crate) dead: bool,
}

impl Default for GroupVitals {
    /// A player whose sim sent no vitals yet: full and alive.
    fn default() -> Self {
        Self {
            health_percent: nw_messages::social::group_data::FULL_PERCENT,
            mana_percent: nw_messages::social::group_data::FULL_PERCENT,
            dead: false,
        }
    }
}

/// The groups state's content, without its list versions.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupsView {
    group: [u8; 16],
    inbound: Vec<GroupInviteData>,
    outbound: Vec<GroupInviteData>,
}

/// The social state's lists.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SocialView {
    friends: Vec<String>,
    friend_invites: Vec<String>,
    blocks: Vec<String>,
}

/// The states last sent to one player in its stay on its sim.
#[derive(Debug, Default)]
pub(crate) struct SentViews {
    /// The last list version used.
    version: u64,
    groups: Option<GroupsView>,
    social: Option<SocialView>,
    guilds: Option<GuildsState>,
    chat: Option<Vec<String>>,
    group_entity: Option<Option<(Uuid, GroupDataState)>>,
}

impl SentViews {
    fn next_version(&mut self) -> u64 {
        self.version = self.version.max(FIRST_VERSION) + 1;
        self.version
    }
}

fn text_of(id: &Uuid) -> String {
    id.hyphenated().to_string()
}

fn texts(ids: &BTreeSet<Uuid>) -> Vec<String> {
    ids.iter().map(text_of).collect()
}

fn encode(write: impl FnOnce(&mut Writer) -> Result<(), MessageError>) -> Option<Vec<u8>> {
    let mut writer = Writer::new();
    match write(&mut writer) {
        Ok(()) => Some(writer.finish()),
        Err(error) => {
            tracing::warn!(%error, "social state not encoded");
            None
        }
    }
}

impl SocialWorld {
    /// Marks `character` for a state check after this input.
    pub(crate) fn touch(&mut self, character: Uuid) {
        self.touched.insert(character);
    }

    /// Forgets what `character` was sent: it arrived on a sim with a new card, or left.
    pub(crate) fn forget_views(&mut self, character: Uuid) {
        self.sent.remove(&character);
        self.touch(character);
    }

    /// The characters whose states an effect changes.
    fn touched_by(&self, effect: &Effect) -> Vec<Uuid> {
        match effect {
            Effect::Store(op) => match op {
                StoreOp::AddLink {
                    character, other, ..
                }
                | StoreOp::RemoveLink {
                    character, other, ..
                } => vec![*character, *other],
                StoreOp::CreateCompany { founder, .. } => vec![*founder],
                StoreOp::UpdateCompany(company) => self.members_of_company(company.id),
                StoreOp::SetMember(member) => vec![member.character],
                StoreOp::RemoveMember { character, .. } => vec![*character],
                StoreOp::AddInvite(invite) => vec![invite.character, invite.inviter],
                StoreOp::RemoveInvite { company, character } => {
                    let mut touched = self.members_of_company(*company);
                    touched.push(*character);
                    touched
                }
                StoreOp::SaveGroup(group) => group.members.clone(),
                StoreOp::Settings { .. }
                | StoreOp::Report(_)
                | StoreOp::AddCoinReturn(_)
                | StoreOp::RemoveCoinReturn(_)
                | StoreOp::DeleteCompany(_)
                | StoreOp::DeleteGroup(_) => Vec::new(),
            },
            // The treasury changes when its change is stored (`treasury_changed` touches).
            Effect::Treasury { .. }
            | Effect::FindAccount { .. }
            | Effect::Deliver { .. }
            | Effect::MoveCoins { .. }
            | Effect::OwnState { .. }
            | Effect::GroupEntity { .. } => Vec::new(),
        }
    }

    fn members_of_company(&self, company: Uuid) -> Vec<Uuid> {
        self.companies
            .get(&company)
            .map(|company| company.members.clone())
            .unwrap_or_default()
    }

    /// Sends every state that changed of the players this input touched, their group and
    /// company included.
    pub(crate) fn refresh_views(&mut self, now_ms: u64) {
        let from_effects: Vec<Uuid> = self
            .out
            .iter()
            .flat_map(|effect| self.touched_by(effect))
            .collect();
        let mut touched = std::mem::take(&mut self.touched);
        touched.extend(from_effects);
        let mut circle: BTreeSet<Uuid> = BTreeSet::new();
        for character in &touched {
            circle.insert(*character);
            let Some(person) = self.people.get(character) else {
                continue;
            };
            if let Some(group) = person.group.and_then(|group| self.groups.get(&group)) {
                circle.extend(group.members.iter().copied());
            }
            if let Some((company, _)) = person.company {
                circle.extend(self.members_of_company(company));
            }
        }
        for character in circle {
            self.refresh_one(character, now_ms);
        }
    }

    fn refresh_one(&mut self, character: Uuid, now_ms: u64) {
        let Some(sim) = self
            .people
            .get(&character)
            .and_then(|person| person.online.as_ref())
            .map(|online| online.sim.clone())
        else {
            return;
        };
        let groups = self.groups_view(character);
        let social = self.social_view(character);
        let guilds = self.guilds_view(character, now_ms);
        let chat = self.chat_view(character);
        let entity = self.group_entity_view(character);
        let mut sent = self.sent.remove(&character).unwrap_or_default();
        let mut out = Vec::new();
        if sent.groups.as_ref() != Some(&groups) {
            let version = sent.next_version();
            let state = GroupsState {
                group_id: Some(groups.group),
                inbound_invites: Some(FullList {
                    version: Some(version),
                    elements: groups.inbound.clone(),
                }),
                outbound_invites: Some(FullList {
                    version: Some(version),
                    elements: groups.outbound.clone(),
                }),
                ..GroupsState::default()
            };
            out.extend(own(character, SocialStateKind::Groups, |w| state.write(w)));
            sent.groups = Some(groups);
        }
        if sent.social.as_ref() != Some(&social) {
            let version = Some(sent.next_version());
            let list = |elements: &[String]| FullList {
                version,
                elements: elements.to_vec(),
            };
            let state = SocialState {
                friends: Some(list(&social.friends)),
                friend_invites: Some(list(&social.friend_invites)),
                social_blocks: Some(list(&social.blocks)),
                ..SocialState::default()
            };
            out.extend(own(character, SocialStateKind::Social, |w| state.write(w)));
            sent.social = Some(social);
        }
        if sent.guilds.as_ref() != Some(&guilds) {
            let state = with_versions(guilds.clone(), sent.next_version());
            out.extend(own(character, SocialStateKind::Guilds, |w| state.write(w)));
            sent.guilds = Some(guilds);
        }
        if sent.chat.as_ref() != Some(&chat) {
            let state = ChatState {
                mutes: Some(FullList {
                    version: Some(sent.next_version()),
                    elements: chat.clone(),
                }),
            };
            out.extend(own(character, SocialStateKind::Chat, |w| state.write(w)));
            sent.chat = Some(chat);
        }
        let entity_out = if sent.group_entity.as_ref() == Some(&entity) {
            None
        } else {
            let view = match &entity {
                Some((group, state)) => encode(|w| state.write(w)).map(|body| {
                    Some(GroupEntityView {
                        group: *group,
                        body,
                    })
                }),
                None => Some(None),
            };
            sent.group_entity = Some(entity);
            view.map(|view| GroupEntityDelivery { character, view })
        };
        self.sent.insert(character, sent);
        for delivery in out {
            self.out.push(Effect::OwnState {
                sim: sim.clone(),
                delivery,
            });
        }
        if let Some(delivery) = entity_out {
            self.out.push(Effect::GroupEntity { sim, delivery });
        }
    }

    fn invite_data(&self, id: [u8; 16], invite: crate::world::GroupInvite) -> GroupInviteData {
        let group = self
            .people
            .get(&invite.inviter)
            .and_then(|person| person.group)
            .map_or(NO_ID, |group| *group.as_bytes());
        GroupInviteData::plain(
            id,
            group,
            CharacterRef(Some(*invite.inviter.as_bytes())),
            CharacterRef(Some(*invite.invitee.as_bytes())),
            invite.created_ms.saturating_mul(NANOS_PER_MS),
        )
    }

    fn groups_view(&self, character: Uuid) -> GroupsView {
        let group = self
            .people
            .get(&character)
            .and_then(|person| person.group)
            .map_or(NO_ID, |group| *group.as_bytes());
        let mut invites: Vec<(&[u8; 16], &crate::world::GroupInvite)> =
            self.group_invites.iter().collect();
        invites.sort_by_key(|(id, invite)| (invite.created_ms, **id));
        let pick = |inbound: bool| -> Vec<GroupInviteData> {
            invites
                .iter()
                .filter(|(_, invite)| {
                    if inbound {
                        invite.invitee == character
                    } else {
                        invite.inviter == character
                    }
                })
                .map(|(id, invite)| self.invite_data(**id, **invite))
                .collect()
        };
        GroupsView {
            group,
            inbound: pick(true),
            outbound: pick(false),
        }
    }

    fn social_view(&self, character: Uuid) -> SocialView {
        self.people.get(&character).map_or(
            SocialView {
                friends: Vec::new(),
                friend_invites: Vec::new(),
                blocks: Vec::new(),
            },
            |person| SocialView {
                friends: texts(&person.friends),
                friend_invites: texts(&person.invites_in),
                blocks: texts(&person.blocks),
            },
        )
    }

    fn chat_view(&self, character: Uuid) -> Vec<String> {
        self.people
            .get(&character)
            .map(|person| texts(&person.mutes))
            .unwrap_or_default()
    }

    /// The company invites `character` got.
    fn company_invites_of(&self, character: Uuid, now_ms: u64) -> Vec<GuildInviteData> {
        let mut invites: Vec<GuildInviteData> = self
            .companies
            .values()
            .filter_map(|company| {
                let inviter = company.invites.get(&character)?;
                let created_ms = self
                    .company_invite_ms
                    .get(&(company.record.id, character))
                    .copied()
                    .unwrap_or(now_ms);
                Some(GuildInviteData {
                    guild: *company.record.id.as_bytes(),
                    name: company.record.name.clone(),
                    crest: crest_of_bytes(&company.record.crest),
                    inviter: text_of(inviter),
                    kind: GUILD_INVITE_KIND,
                    inviter_platform: NO_PLATFORM_ID.to_owned(),
                    created_ns: created_ms.saturating_mul(NANOS_PER_MS),
                })
            })
            .collect();
        invites.sort_by(|left, right| {
            (left.created_ns, left.guild).cmp(&(right.created_ns, right.guild))
        });
        invites
    }

    fn guilds_view(&self, character: Uuid, now_ms: u64) -> GuildsState {
        let Some(person) = self.people.get(&character) else {
            return GuildsState::new_character();
        };
        let last_leave_ns = person
            .settings
            .company_left_at_ms
            .map_or(0, |at_ms| at_ms.saturating_mul(NANOS_PER_MS));
        let invites = Some(FullList {
            version: None,
            elements: self.company_invites_of(character, now_ms),
        });
        fn empty_list<T>() -> Option<FullList<T>> {
            Some(FullList {
                version: None,
                elements: Vec::new(),
            })
        }
        let Some((company, rank)) = person
            .company
            .and_then(|(id, rank)| self.companies.get(&id).map(|company| (company, rank)))
        else {
            return GuildsState {
                guild_id: Some(NO_ID),
                rank: Some(NO_GUILD_RANK),
                last_leave_ns: Some(last_leave_ns),
                member_ids: empty_list(),
                member_ranks: empty_list(),
                member_online: empty_list(),
                member_last_online_ns: empty_list(),
                invites,
                ..GuildsState::default()
            };
        };
        let record = &company.record;
        let members = &company.members;
        let member = |character: &Uuid| self.people.get(character);
        GuildsState {
            guild_id: Some(*record.id.as_bytes()),
            rank: Some(u16::from(rank)),
            using_gm_commands: Some(false),
            forced_rename: Some(false),
            motd: Some(record.motd.clone()),
            motd_author: Some(CharacterRef(
                record.motd_author.map(|author| *author.as_bytes()),
            )),
            motd_time_ns: Some(record.motd_at_ms.unwrap_or(0).saturating_mul(NANOS_PER_MS)),
            siege_hour: Some(record.siege_hour),
            treasury: Some(record.treasury),
            withdrawal_limit: record.withdrawal_limit,
            last_leave_ns: Some(last_leave_ns),
            withdrawn_today: Some(self.withdrawn_today(character, now_ms)),
            seat_overthrowable: Some(false),
            seat_vacant: Some(false),
            member_ids: Some(FullList {
                version: None,
                elements: members.iter().map(text_of).collect(),
            }),
            member_ranks: Some(FullList {
                version: None,
                elements: members
                    .iter()
                    .map(|character| {
                        member(character)
                            .and_then(|person| person.company)
                            .map_or(NO_GUILD_RANK, |(_, rank)| u16::from(rank))
                    })
                    .collect(),
            }),
            member_online: Some(FullList {
                version: None,
                elements: members
                    .iter()
                    .map(|character| {
                        if member(character).is_some_and(|person| person.online.is_some()) {
                            ONLINE
                        } else {
                            OFFLINE
                        }
                    })
                    .collect(),
            }),
            member_last_online_ns: Some(FullList {
                version: None,
                elements: members
                    .iter()
                    .map(|character| {
                        member(character).map_or(ONLINE_LAST_SEEN, |person| {
                            if person.online.is_some() {
                                ONLINE_LAST_SEEN
                            } else {
                                person
                                    .settings
                                    .last_online_ms
                                    .unwrap_or(0)
                                    .saturating_mul(NANOS_PER_MS)
                            }
                        })
                    })
                    .collect(),
            }),
            invites,
            outstanding_invites: Some(company.invites.len() as u64),
            ..GuildsState::default()
        }
    }

    fn group_entity_view(&self, character: Uuid) -> Option<(Uuid, GroupDataState)> {
        let group = self
            .people
            .get(&character)
            .and_then(|person| person.group)
            .and_then(|group| self.groups.get(&group))?;
        let members: Vec<GroupMemberView> = group
            .members
            .iter()
            .filter_map(|member| {
                let person = self.people.get(member)?;
                Some(GroupMemberView {
                    character: *member.as_bytes(),
                    name: person.name.clone(),
                    icon: person
                        .settings
                        .icon
                        .map(|icon| icon_of_bytes(&icon))
                        .unwrap_or_default(),
                    health: person.vitals.health_percent,
                    mana: person.vitals.mana_percent,
                    connected: person.online.is_some(),
                    dead: person.vitals.dead,
                    leader: *member == group.leader,
                    faction: person.settings.faction,
                    level: u8::try_from(person.settings.level).unwrap_or(u8::MAX),
                })
            })
            .collect();
        Some((
            group.id,
            GroupDataState::whole(*group.id.as_bytes(), &members),
        ))
    }
}

/// The company state with `version` on each of its lists.
fn with_versions(mut state: GuildsState, version: u64) -> GuildsState {
    let version = Some(version);
    if let Some(list) = &mut state.member_ids {
        list.version = version;
    }
    if let Some(list) = &mut state.member_ranks {
        list.version = version;
    }
    if let Some(list) = &mut state.member_online {
        list.version = version;
    }
    if let Some(list) = &mut state.member_last_online_ns {
        list.version = version;
    }
    if let Some(list) = &mut state.invites {
        list.version = version;
    }
    state
}

fn own(
    character: Uuid,
    kind: SocialStateKind,
    write: impl FnOnce(&mut Writer) -> Result<(), MessageError>,
) -> Option<OwnStateDelivery> {
    encode(write).map(|body| OwnStateDelivery {
        character,
        kind,
        body,
    })
}

/// The group vitals of one input.
pub(crate) fn vitals_of(input: &nw_bus::PlayerVitals) -> GroupVitals {
    GroupVitals {
        health_percent: input.health_percent,
        mana_percent: input.mana_percent,
        dead: input.dead,
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::payload::MemberState;
    use nw_messages::social::group_data::{FLAG_CONNECTED, FLAG_LEADER, GROUP_DATA_TYPE_INDEX};
    use nw_messages::social::groups::{
        GROUP_INVITE_TYPE_INDEX, REPLY_ACCEPT, REPLY_TO_INVITE_TYPE_INDEX,
    };
    use nw_messages::social::states::GROUPS_STATE_TYPE_INDEX;

    use super::*;
    use crate::world::tests::{NOW_MS, call, character, join, text, world};

    fn own_states(effects: &[Effect], to: Uuid, kind: SocialStateKind) -> Vec<Vec<u8>> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::OwnState { delivery, .. }
                    if delivery.character == to && delivery.kind == kind =>
                {
                    Some(delivery.body.clone())
                }
                _ => None,
            })
            .collect()
    }

    fn entities(effects: &[Effect], to: Uuid) -> Vec<Option<GroupEntityView>> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::GroupEntity { delivery, .. } if delivery.character == to => {
                    Some(delivery.view.clone())
                }
                _ => None,
            })
            .collect()
    }

    fn groups_state(body: &[u8]) -> GroupsState {
        match MemberState::decode(GROUPS_STATE_TYPE_INDEX, body).expect("decodes") {
            MemberState::Groups(state) => state,
            other => panic!("a groups state: {other:?}"),
        }
    }

    #[test]
    fn a_login_sends_every_state_once_and_then_only_changes() {
        let mut world = world();
        let effects = join(&mut world, 1, "Ann", "sim-a");
        for kind in [
            SocialStateKind::Groups,
            SocialStateKind::Social,
            SocialStateKind::Guilds,
            SocialStateKind::Chat,
        ] {
            assert_eq!(
                own_states(&effects, character(1), kind).len(),
                1,
                "{kind:?}"
            );
        }
        assert_eq!(entities(&effects, character(1)), [None], "no group");
        let again = world.input(
            &crate::world::tests::sim("sim-a"),
            nw_bus::SocialInput::Updated(crate::world::tests::player(1, "Ann", "sim-a")),
            NOW_MS,
        );
        assert!(
            again
                .iter()
                .all(|effect| !matches!(effect, Effect::OwnState { .. })),
            "nothing changed"
        );
    }

    #[test]
    fn an_invite_shows_to_both_and_a_join_makes_the_group_entity() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-b");
        let mut args = text(&character(2).hyphenated().to_string());
        args.extend([0; 17]);
        let effects = call(
            &mut world,
            1,
            "sim-a",
            GROUP_INVITE_TYPE_INDEX,
            &args,
            NOW_MS,
        );
        let inbound = own_states(&effects, character(2), SocialStateKind::Groups);
        let [inbound] = inbound.as_slice() else {
            panic!("one groups state for the invitee: {inbound:?}");
        };
        let state = groups_state(inbound);
        let invites = state.inbound_invites.expect("inbound").elements;
        assert_eq!(invites.len(), 1);
        assert_eq!(
            invites[0].inviter,
            CharacterRef(Some(*character(1).as_bytes()))
        );
        assert_eq!(invites[0].created_ns, NOW_MS * NANOS_PER_MS);
        let outbound = own_states(&effects, character(1), SocialStateKind::Groups);
        assert_eq!(
            groups_state(&outbound[0])
                .outbound_invites
                .expect("outbound")
                .elements
                .len(),
            1
        );
        let mut reply = invites[0].invite.to_vec();
        reply.extend(REPLY_ACCEPT.to_be_bytes());
        let effects = call(
            &mut world,
            2,
            "sim-b",
            REPLY_TO_INVITE_TYPE_INDEX,
            &reply,
            NOW_MS,
        );
        let group = world.people[&character(2)].group.expect("a group");
        let joined = groups_state(&own_states(&effects, character(2), SocialStateKind::Groups)[0]);
        assert_eq!(joined.group_id, Some(*group.as_bytes()));
        assert_eq!(joined.inbound_invites.expect("inbound").elements.len(), 0);
        for member in [character(1), character(2)] {
            let views = entities(&effects, member);
            let [Some(view)] = views.as_slice() else {
                panic!("one group entity: {views:?}");
            };
            assert_eq!(view.group, group);
            let MemberState::GroupData(state) =
                MemberState::decode(GROUP_DATA_TYPE_INDEX, &view.body).expect("decodes")
            else {
                panic!("group data");
            };
            assert_eq!(
                state.members[0].flags,
                Some([FLAG_CONNECTED | FLAG_LEADER, 0])
            );
            assert_eq!(state.members[1].flags, Some([FLAG_CONNECTED, 0]));
        }
    }

    #[test]
    fn group_vitals_change_the_group_entity_of_every_member() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-a");
        let mut args = text(&character(2).hyphenated().to_string());
        args.extend([0; 17]);
        call(
            &mut world,
            1,
            "sim-a",
            GROUP_INVITE_TYPE_INDEX,
            &args,
            NOW_MS,
        );
        let invite = *world.group_invites.keys().next().expect("invite");
        let mut reply = invite.to_vec();
        reply.extend(REPLY_ACCEPT.to_be_bytes());
        call(
            &mut world,
            2,
            "sim-a",
            REPLY_TO_INVITE_TYPE_INDEX,
            &reply,
            NOW_MS,
        );
        let effects = world.input(
            &crate::world::tests::sim("sim-a"),
            nw_bus::SocialInput::Vitals(nw_bus::PlayerVitals {
                character: character(2),
                health_percent: 40,
                mana_percent: 100,
                dead: false,
            }),
            NOW_MS,
        );
        for member in [character(1), character(2)] {
            assert_eq!(entities(&effects, member).len(), 1, "{member}");
        }
    }
}
