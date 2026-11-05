// SPDX-License-Identifier: AGPL-3.0-or-later
//! Groups: invites, answers, leaving, the leader, and the group's voice channel, across sims.
//!
//! The retail flow of the captures (see `nw_messages::social::groups`): an invite of an offline
//! player gets a 3453; when an invite ends, the inviter and the invitee each get a 1677; a
//! member who leaves gets a 5283 for each other member and each other member one for it. A group
//! of one member ends. An invite ends when its inviter or invitee goes offline.
//!
//! The client shows a group from the `GroupsComponentReplicatedState` (`groupId`,
//! `inboundGroupInvites`, `outboundGroupInvites`) and the group entity
//! (`GroupDataComponentReplicatedState`), which [`crate::views`] sends. The kick vote (3498)
//! stays refused: no capture holds a vote, and its messages (`OnKickVoteInitiated`,
//! `OnKickVoteEnded`) have no captured layout.

use nw_bus::GroupAnswer;
use nw_db::SocialGroup;
use nw_messages::social::groups::{
    ERROR_INVITEE_OFFLINE, INVITE_ENDED_ANSWERED, REMOVED_LEFT, REPLY_ACCEPT,
};
use nw_messages::social::{CharacterRef, GroupCall, GroupPush, SocialPush};
use uuid::Uuid;

use crate::effects::StoreOp;
use crate::voice::new_group_channel;
use crate::world::{GroupInvite, METRIC_CALLS, REFUSED, RESULT_LABEL, SocialWorld};

/// Most members of a group: the five of an expedition group (Inferred; the largest retail group
/// of the captures, `dungeon_dynasty_tank_`, had four members at its end).
pub(crate) const GROUP_MEMBER_LIMIT: usize = 5;
/// The last byte of every retail 1677.
const INVITE_REMOVED_FLAG: u8 = 0;
/// The 3453 error of an invite that the rules refuse (the invitee blocks the inviter, is in a
/// group, or the group is full) and of an accepted invite whose group filled up. Inferred: the
/// only retail 3453 error of the captures is [`ERROR_INVITEE_OFFLINE`], and the client's other
/// error values are Unknown; it also tells a blocked inviter nothing more than an offline
/// invitee would.
const ERROR_INVITE_REFUSED: u32 = ERROR_INVITEE_OFFLINE;

fn reference(character: Uuid) -> CharacterRef {
    CharacterRef(Some(*character.as_bytes()))
}

impl SocialWorld {
    /// The group of `character` for the director's [`nw_bus::GroupQuery`] (group expeditions):
    /// its id, leader and every member, the character included; [`GroupAnswer::Alone`] when it is
    /// in no group.
    pub fn group_of(&self, character: Uuid) -> GroupAnswer {
        self.people
            .get(&character)
            .and_then(|person| person.group)
            .and_then(|group| self.groups.get(&group))
            .filter(|group| group.members.contains(&character))
            .map_or(GroupAnswer::Alone, |group| GroupAnswer::Group {
                group: group.id,
                leader: group.leader,
                members: group.members.clone(),
            })
    }

    pub(crate) fn group_call(&mut self, character: Uuid, call: GroupCall, now_ms: u64) {
        match call {
            GroupCall::Invite {
                character: invitee, ..
            } => self.invite_to_group(character, &invitee, now_ms),
            GroupCall::Reply { invite, answer } => self.reply_to_group(character, invite, answer),
            GroupCall::Leave { .. } => self.leave_group(character),
            GroupCall::SetLeader { character: leader } => self.set_leader(character, &leader),
            GroupCall::KickVote { .. } => {
                // The vote messages are not in the captures, so there is no vote.
                metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            }
            GroupCall::Ping { data } => self.group_ping(character, data),
            GroupCall::MoveRaidMember {
                character: member,
                group,
            } => {
                self.move_raid_member(character, &member, group);
            }
            GroupCall::SwapRaidMembers { first, second } => {
                self.swap_raid_members(character, &first, &second);
            }
        }
    }

    /// A ping of `character` goes to every other member of its group, as it came.
    fn group_ping(
        &mut self,
        character: Uuid,
        data: [u8; nw_messages::social::groups::PING_DATA_LEN],
    ) {
        let Some(others) = self.other_members(character) else {
            return;
        };
        self.push(&others, SocialPush::Group(GroupPush::Ping { data }));
    }

    /// The members of the group of `character` but `character`; `None` in no group.
    fn other_members(&self, character: Uuid) -> Option<Vec<Uuid>> {
        let group = self
            .people
            .get(&character)
            .and_then(|person| person.group)
            .and_then(|group| self.groups.get(&group))?;
        Some(
            group
                .members
                .iter()
                .copied()
                .filter(|member| *member != character)
                .collect(),
        )
    }

    /// Moves a raid member to the first place of raid group `group` (the members in join
    /// order, [`GROUP_MEMBER_LIMIT`] per group) and saves the group; every member's group
    /// state goes out again. A group this raid does not have is refused.
    fn move_raid_member(&mut self, character: Uuid, member: &str, group: u8) {
        let Some(member) = Uuid::parse_str(member).ok() else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        let Some(group_id) = self.people.get(&character).and_then(|person| person.group) else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        let Some(raid) = self.groups.get_mut(&group_id) else {
            return;
        };
        let Some(from) = raid.members.iter().position(|known| *known == member) else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        // Raid groups are 1-based in the retail calls (1 and 2 and 3 seen).
        let Some(first) = usize::from(group.saturating_sub(1)).checked_mul(GROUP_MEMBER_LIMIT)
        else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        if group == 0 || first >= raid.members.len() {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        }
        raid.members.remove(from);
        let to = first.min(raid.members.len());
        raid.members.insert(to, member);
        let saved = raid.clone();
        let members = saved.members.clone();
        self.store(StoreOp::SaveGroup(saved));
        for member in &members {
            self.touch(*member);
        }
    }

    /// Swaps the places of two raid members and saves the group; every member's group state
    /// goes out again.
    fn swap_raid_members(&mut self, character: Uuid, first: &str, second: &str) {
        let (Ok(first), Ok(second)) = (Uuid::parse_str(first), Uuid::parse_str(second)) else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        let Some(group_id) = self.people.get(&character).and_then(|person| person.group) else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        let Some(raid) = self.groups.get_mut(&group_id) else {
            return;
        };
        let (Some(one), Some(two)) = (
            raid.members.iter().position(|known| *known == first),
            raid.members.iter().position(|known| *known == second),
        ) else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        raid.members.swap(one, two);
        let saved = raid.clone();
        let members = saved.members.clone();
        self.store(StoreOp::SaveGroup(saved));
        for member in &members {
            self.touch(*member);
        }
    }

    fn invite_to_group(&mut self, inviter: Uuid, invitee: &str, now_ms: u64) {
        let Some(target) = self.known(invitee).filter(|target| *target != inviter) else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        if !self.is_online(target) {
            let push = GroupPush::Error {
                error: ERROR_INVITEE_OFFLINE,
                character: reference(target),
            };
            self.push(&[inviter], SocialPush::Group(push));
            return;
        }
        let blocked = self
            .people
            .get(&target)
            .is_some_and(|person| person.blocks.contains(&inviter));
        let target_grouped = self
            .people
            .get(&target)
            .is_some_and(|person| person.group.is_some());
        let full = self
            .people
            .get(&inviter)
            .and_then(|person| person.group)
            .and_then(|group| self.groups.get(&group))
            .is_some_and(|group| {
                group.leader != inviter || group.members.len() >= GROUP_MEMBER_LIMIT
            });
        if blocked || target_grouped || full {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            self.refuse_group(inviter, target);
            return;
        }
        let mut id = [0; 16];
        id.copy_from_slice(Uuid::new_v4().as_bytes());
        self.group_invites.insert(
            id,
            GroupInvite {
                inviter,
                invitee: target,
                created_ms: now_ms,
            },
        );
        // Both see it in their groups state: the invitee's popup, the inviter's sent invites.
        self.touch(inviter);
        self.touch(target);
    }

    fn reply_to_group(&mut self, invitee: Uuid, invite: [u8; 16], answer: u32) {
        let Some(open) = self
            .group_invites
            .get(&invite)
            .copied()
            .filter(|open| open.invitee == invitee)
        else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        self.end_group_invite(invite, open);
        if answer == REPLY_ACCEPT {
            self.join_group(open.inviter, invitee);
        }
    }

    /// Ends invite `id`: both sides get the retail 1677.
    fn end_group_invite(&mut self, id: [u8; 16], invite: GroupInvite) {
        self.group_invites.remove(&id);
        self.touch(invite.inviter);
        self.touch(invite.invitee);
        for (told, other, outbound) in [
            (invite.inviter, invite.invitee, true),
            (invite.invitee, invite.inviter, false),
        ] {
            let push = GroupPush::InviteRemoved {
                invite: id,
                other: reference(other),
                kind: INVITE_ENDED_ANSWERED,
                second: CharacterRef(None),
                outbound,
                flag: INVITE_REMOVED_FLAG,
            };
            self.push(&[told], SocialPush::Group(push));
        }
    }

    /// Ends the open invites of `character` (it went offline).
    pub(crate) fn drop_group_invites_of(&mut self, character: Uuid) {
        let ended: Vec<([u8; 16], GroupInvite)> = self
            .group_invites
            .iter()
            .filter(|(_, invite)| invite.inviter == character || invite.invitee == character)
            .map(|(id, invite)| (*id, *invite))
            .collect();
        for (id, invite) in ended {
            self.end_group_invite(id, invite);
        }
    }

    /// Tells `told` that its group step with `other` was refused (3453).
    fn refuse_group(&mut self, told: Uuid, other: Uuid) {
        let push = GroupPush::Error {
            error: ERROR_INVITE_REFUSED,
            character: reference(other),
        };
        self.push(&[told], SocialPush::Group(push));
    }

    fn join_group(&mut self, inviter: Uuid, invitee: Uuid) {
        let invitee_grouped = self
            .people
            .get(&invitee)
            .is_some_and(|person| person.group.is_some());
        let inviter_group_full = self
            .people
            .get(&inviter)
            .and_then(|person| person.group)
            .and_then(|group| self.groups.get(&group))
            .is_some_and(|group| group.members.len() >= GROUP_MEMBER_LIMIT);
        if invitee_grouped || inviter_group_full {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            self.refuse_group(invitee, inviter);
            return;
        }
        let group_id = match self.people.get(&inviter).and_then(|person| person.group) {
            Some(group) => group,
            None => {
                let group = SocialGroup {
                    id: Uuid::new_v4(),
                    leader: inviter,
                    voice_channel: new_group_channel(),
                    members: vec![inviter],
                };
                let id = group.id;
                self.groups.insert(id, group);
                if let Some(person) = self.people.get_mut(&inviter) {
                    person.group = Some(id);
                }
                id
            }
        };
        let Some(group) = self.groups.get_mut(&group_id) else {
            return;
        };
        group.members.push(invitee);
        let saved = group.clone();
        if let Some(person) = self.people.get_mut(&invitee) {
            person.group = Some(group_id);
        }
        self.store(StoreOp::SaveGroup(saved));
    }

    pub(crate) fn leave_group(&mut self, character: Uuid) {
        let Some(group_id) = self.people.get(&character).and_then(|person| person.group) else {
            return;
        };
        let Some(group) = self.groups.get_mut(&group_id) else {
            return;
        };
        group.members.retain(|member| *member != character);
        let others = group.members.clone();
        if group.leader == character
            && let Some(next) = others.first()
        {
            group.leader = *next;
        }
        let saved = group.clone();
        if let Some(person) = self.people.get_mut(&character) {
            person.group = None;
        }
        // Its group entity goes and its group id clears.
        self.touch(character);
        for other in &others {
            let push = GroupPush::MemberRemoved {
                character: reference(*other),
                reason: REMOVED_LEFT,
            };
            self.push(&[character], SocialPush::Group(push));
        }
        let push = GroupPush::MemberRemoved {
            character: reference(character),
            reason: REMOVED_LEFT,
        };
        self.push(&others, SocialPush::Group(push));
        if others.len() <= 1 {
            for other in &others {
                if let Some(person) = self.people.get_mut(other) {
                    person.group = None;
                }
                self.touch(*other);
            }
            self.groups.remove(&group_id);
            self.store(StoreOp::DeleteGroup(group_id));
        } else {
            self.store(StoreOp::SaveGroup(saved));
        }
    }

    fn set_leader(&mut self, character: Uuid, leader: &str) {
        let Some(leader) = Uuid::parse_str(leader).ok() else {
            return;
        };
        let Some(group) = self
            .people
            .get(&character)
            .and_then(|person| person.group)
            .and_then(|group| self.groups.get_mut(&group))
            .filter(|group| group.leader == character && group.members.contains(&leader))
        else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            return;
        };
        group.leader = leader;
        let saved = group.clone();
        self.store(StoreOp::SaveGroup(saved));
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::social::ChatChannel;
    use nw_messages::social::chat::{
        RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX, SEND_CHAT_MESSAGE_TYPE_INDEX,
    };
    use nw_messages::social::groups::{
        GROUP_ERROR_TYPE_INDEX, GROUP_INVITE_TYPE_INDEX, INVITE_REMOVED_TYPE_INDEX,
        LEAVE_GROUP_TYPE_INDEX, MEMBER_REMOVED_TYPE_INDEX, REPLY_TO_INVITE_TYPE_INDEX,
    };

    use super::*;
    use crate::Effect;
    use crate::world::tests::{NOW_MS, call, character, join, pushes, text, world};

    fn invite_args(number: u128) -> Vec<u8> {
        let mut args = text(&character(number).hyphenated().to_string());
        args.extend([0; 17]);
        args
    }

    #[test]
    fn an_accepted_invite_makes_a_group_across_sims_and_leaving_ends_it() {
        let mut world = world();
        let effects = {
            join(&mut world, 1, "Ann", "sim-a");
            call(
                &mut world,
                1,
                "sim-a",
                GROUP_INVITE_TYPE_INDEX,
                &invite_args(2),
                NOW_MS,
            )
        };
        assert_eq!(
            pushes(&effects, GROUP_ERROR_TYPE_INDEX).len(),
            1,
            "Bob is offline"
        );
        join(&mut world, 2, "Bob", "sim-b");
        call(
            &mut world,
            1,
            "sim-a",
            GROUP_INVITE_TYPE_INDEX,
            &invite_args(2),
            NOW_MS,
        );
        let invite = *world.group_invites.keys().next().expect("an open invite");
        let mut reply = invite.to_vec();
        reply.extend(REPLY_ACCEPT.to_be_bytes());
        let effects = call(
            &mut world,
            2,
            "sim-b",
            REPLY_TO_INVITE_TYPE_INDEX,
            &reply,
            NOW_MS,
        );
        assert_eq!(pushes(&effects, INVITE_REMOVED_TYPE_INDEX).len(), 2);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Store(StoreOp::SaveGroup(_))))
        );
        let group = world.people[&character(2)].group.expect("Bob's group");
        assert_eq!(world.people[&character(1)].group, Some(group));
        let mut line = ChatChannel::Group.wire().to_be_bytes().to_vec();
        line.extend(text("hi"));
        line.extend(text(""));
        line.push(0);
        let heard = call(
            &mut world,
            1,
            "sim-a",
            SEND_CHAT_MESSAGE_TYPE_INDEX,
            &line,
            NOW_MS,
        );
        assert_eq!(
            pushes(&heard, RECEIVE_BATCHED_CHAT_MESSAGES_TYPE_INDEX).len(),
            2,
            "two sims"
        );
        let effects = call(
            &mut world,
            2,
            "sim-b",
            LEAVE_GROUP_TYPE_INDEX,
            &[0; 4],
            NOW_MS,
        );
        assert_eq!(pushes(&effects, MEMBER_REMOVED_TYPE_INDEX).len(), 2);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Store(StoreOp::DeleteGroup(_))))
        );
        assert!(world.groups.is_empty());
        assert_eq!(world.people[&character(1)].group, None);
    }

    fn group_error(effects: &[Effect]) -> Vec<(Vec<Uuid>, SocialPush)> {
        pushes(effects, GROUP_ERROR_TYPE_INDEX)
    }

    fn refused(to: u128, other: u128) -> (Vec<Uuid>, SocialPush) {
        (
            vec![character(to)],
            SocialPush::Group(GroupPush::Error {
                error: ERROR_INVITE_REFUSED,
                character: reference(character(other)),
            }),
        )
    }

    #[test]
    fn a_refused_invite_and_an_accept_into_a_full_group_are_answered() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-a");
        join(&mut world, 3, "Cid", "sim-a");
        let invite = |world: &mut SocialWorld, from: u128, to: u128| {
            call(
                world,
                from,
                "sim-a",
                GROUP_INVITE_TYPE_INDEX,
                &invite_args(to),
                NOW_MS,
            )
        };
        world
            .people
            .get_mut(&character(2))
            .expect("Bob")
            .blocks
            .insert(character(1));
        assert_eq!(
            group_error(&invite(&mut world, 1, 2)),
            [refused(1, 2)],
            "blocked"
        );
        assert!(world.group_invites.is_empty());
        // Cid invites Ann; Bob's own group (Bob and Cid's) fills up before Ann answers.
        invite(&mut world, 3, 1);
        let open = *world.group_invites.keys().next().expect("an open invite");
        let full = SocialGroup {
            id: Uuid::from_u128(900),
            leader: character(3),
            voice_channel: String::new(),
            members: (3..3 + GROUP_MEMBER_LIMIT as u128).map(character).collect(),
        };
        for member in &full.members {
            if let Some(person) = world.people.get_mut(member) {
                person.group = Some(full.id);
            }
        }
        world.groups.insert(full.id, full);
        let mut reply = open.to_vec();
        reply.extend(REPLY_ACCEPT.to_be_bytes());
        let effects = call(
            &mut world,
            1,
            "sim-a",
            REPLY_TO_INVITE_TYPE_INDEX,
            &reply,
            NOW_MS,
        );
        assert_eq!(group_error(&effects), [refused(1, 3)], "the group is full");
        assert_eq!(world.people[&character(1)].group, None, "no silent join");
        assert_eq!(
            group_error(&invite(&mut world, 1, 3)),
            [refused(1, 3)],
            "Cid is in a group"
        );
    }

    #[test]
    fn the_group_query_names_the_group_its_leader_and_every_member() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-b");
        assert_eq!(world.group_of(character(1)), GroupAnswer::Alone);
        call(
            &mut world,
            1,
            "sim-a",
            GROUP_INVITE_TYPE_INDEX,
            &invite_args(2),
            NOW_MS,
        );
        let invite = *world.group_invites.keys().next().expect("an open invite");
        let mut reply = invite.to_vec();
        reply.extend(REPLY_ACCEPT.to_be_bytes());
        call(
            &mut world,
            2,
            "sim-b",
            REPLY_TO_INVITE_TYPE_INDEX,
            &reply,
            NOW_MS,
        );
        let group = world.people[&character(2)].group.expect("Bob's group");
        let expected = GroupAnswer::Group {
            group,
            leader: character(1),
            members: vec![character(1), character(2)],
        };
        assert_eq!(world.group_of(character(1)), expected);
        assert_eq!(world.group_of(character(2)), expected);
        assert_eq!(world.group_of(character(3)), GroupAnswer::Alone);
    }

    #[test]
    fn a_ping_reaches_the_other_members_and_raid_moves_change_the_order() {
        use nw_messages::social::groups::{
            GROUP_PING_TYPE_INDEX, MOVE_RAID_MEMBER_TYPE_INDEX, PING_DATA_LEN,
            SWAP_RAID_MEMBERS_TYPE_INDEX,
        };

        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-a");
        join(&mut world, 3, "Cid", "sim-a");
        for invitee in [2, 3] {
            call(
                &mut world,
                1,
                "sim-a",
                GROUP_INVITE_TYPE_INDEX,
                &invite_args(invitee),
                NOW_MS,
            );
            let invite = *world.group_invites.keys().next().expect("an open invite");
            let mut reply = invite.to_vec();
            reply.extend(REPLY_ACCEPT.to_be_bytes());
            call(
                &mut world,
                invitee,
                "sim-a",
                REPLY_TO_INVITE_TYPE_INDEX,
                &reply,
                NOW_MS,
            );
        }
        let group = world.people[&character(1)].group.expect("a group");
        assert_eq!(
            world.groups[&group].members,
            [character(1), character(2), character(3)]
        );
        let effects = call(
            &mut world,
            1,
            "sim-a",
            GROUP_PING_TYPE_INDEX,
            &[0; PING_DATA_LEN],
            NOW_MS,
        );
        let pings = pushes(&effects, GROUP_PING_TYPE_INDEX);
        assert_eq!(pings.len(), 1);
        assert_eq!(pings[0].0, [character(2), character(3)]);
        assert_eq!(
            pings[0].1,
            SocialPush::Group(GroupPush::Ping {
                data: [0; PING_DATA_LEN]
            })
        );
        let swap = [
            text(&character(2).hyphenated().to_string()),
            text(&character(3).hyphenated().to_string()),
        ]
        .concat();
        call(
            &mut world,
            1,
            "sim-a",
            SWAP_RAID_MEMBERS_TYPE_INDEX,
            &swap,
            NOW_MS,
        );
        assert_eq!(
            world.groups[&group].members,
            [character(1), character(3), character(2)]
        );
        let mut move_args = text(&character(2).hyphenated().to_string());
        move_args.push(1);
        call(
            &mut world,
            1,
            "sim-a",
            MOVE_RAID_MEMBER_TYPE_INDEX,
            &move_args,
            NOW_MS,
        );
        assert_eq!(
            world.groups[&group].members,
            [character(2), character(1), character(3)],
            "group 1 starts at the first member"
        );
    }

    #[test]
    fn an_invite_ends_when_its_invitee_goes_offline() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-a");
        call(
            &mut world,
            1,
            "sim-a",
            GROUP_INVITE_TYPE_INDEX,
            &invite_args(2),
            NOW_MS,
        );
        let effects = world.sim_lost(&crate::world::tests::sim("sim-a"), NOW_MS);
        assert!(world.group_invites.is_empty());
        assert!(pushes(&effects, INVITE_REMOVED_TYPE_INDEX).len() <= 2);
    }
}
