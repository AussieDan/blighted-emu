// SPDX-License-Identifier: AGPL-3.0-or-later
//! The players calls (search, phases, options, blocks, reports, friends) and the voice calls.
//!
//! Friends (6533, the client's `FriendRequestTypes`): an invite is stored until the other player
//! accepts or rejects it; an invite to a player who already invited the sender makes them
//! friends at once. The friend list reaches the client in the `SocialReplicatedState`
//! (`friends`, `friendInvites`, `socialBlocks`: [`crate::views`]), and both sides get the
//! friend's online notice (4128) at once.

use nw_db::{LinkKind, PlayerReport};
use nw_messages::FacetHeader;
use nw_messages::player_data::{
    MAX_REQUESTED_PLAYERS, NO_GUILD, NO_PLATFORM, NO_PLATFORM_PRESENCE,
};
use nw_messages::social::players::{
    FAILURE_NO_DATA, ONLINE_STATE_OFFLINE, ONLINE_STATE_ONLINE, PHASE_ID_TRAILER,
};
use nw_messages::social::voice::{
    CHANNEL_POSITIONAL, REGISTERED_FLAG, TOKEN_JOIN, TOKEN_LOGIN, TOKEN_TRANSCRIPTION,
};
use nw_messages::social::{
    CharacterRef, FriendRequest, PlayerCall, PlayerPush, RequestFailedMsg, SocialPush, VoiceCall,
    VoicePush, icon_of_bytes,
};
use nw_messages::{PlayerDataResponseMsg, PlayerDataView};
use uuid::Uuid;

use crate::effects::StoreOp;
use crate::privacy::Audience;
use crate::voice::{TokenAction, VoiceService};
use crate::world::{METRIC_CALLS, REFUSED, RESULT_LABEL, SocialWorld};

/// The pronoun of a character of this server: it has no pronoun choice (0 is the value of 251
/// of 350 retail player data entries).
const DEFAULT_PRONOUN: u8 = 0;
/// The 6675 of a player in no phase (retail `Hive 3v3 group`: every field 0).
const NO_PHASE_STATE: u8 = 0;
/// The phase of a player in the open world (retail: 0 outside instances); an offline player's
/// phase too (Inferred: no capture asks for an offline player). A player on an instance sim is in
/// that instance's phase, from the sim's heartbeat.
const OPEN_WORLD_PHASE: u64 = 0;
/// Milliseconds per second, for token times.
const MS_PER_SECOND: u64 = 1000;
/// Metric: voice calls with no voice service.
const NO_VOICE: &str = "no_voice";

impl SocialWorld {
    pub(crate) fn player_call(&mut self, character: Uuid, header: FacetHeader, call: PlayerCall) {
        match call {
            PlayerCall::Search {
                request,
                name,
                limit,
            } => self.search(character, header, request, &name, limit),
            PlayerCall::PhaseId {
                character: other, ..
            } => {
                if let Ok(other) = Uuid::parse_str(&other) {
                    let push = PlayerPush::PhaseId {
                        character: CharacterRef(Some(*other.as_bytes())),
                        phase: self.phase_of(other).unwrap_or(OPEN_WORLD_PHASE),
                        trailer: PHASE_ID_TRAILER,
                    };
                    self.answer(character, header, SocialPush::Player(push));
                }
            }
            PlayerCall::PhaseData {
                character: other, ..
            } => {
                if let Ok(other) = Uuid::parse_str(&other) {
                    // The state byte and the two counts of a player in an instance are
                    // Unknown (no capture asks for one): they stay those of no phase.
                    let push = PlayerPush::PhaseData {
                        character: CharacterRef(Some(*other.as_bytes())),
                        state: NO_PHASE_STATE,
                        first: 0,
                        second: 0,
                        sector: self.phase_of(other).unwrap_or(OPEN_WORLD_PHASE),
                    };
                    self.answer(character, header, SocialPush::Player(push));
                }
            }
            PlayerCall::StreamerMode { settings } => {
                if let Some(person) = self.people.get_mut(&character) {
                    person.settings.streamer_mode = settings;
                }
                self.store_settings(character);
            }
            PlayerCall::Crossplay { permissions } => {
                if let Some(person) = self.people.get_mut(&character) {
                    person.settings.crossplay = Some(permissions);
                }
                self.store_settings(character);
            }
            PlayerCall::ChattingState { state } => {
                // Others see it in the `SocialReplicatedState` (`chattingStateMessageType`),
                // whose layout is Unknown.
                tracing::debug!(%character, state, "chatting state");
            }
            PlayerCall::Block { character: other } => self.set_mute(character, &other, true, true),
            PlayerCall::Unblock { character: other } => {
                self.set_mute(character, &other, false, true);
            }
            PlayerCall::Report {
                reason,
                character: reported,
                comment,
                name,
                category,
                detail,
                locale,
            } => match Uuid::parse_str(&reported) {
                Ok(reported) => self.store(StoreOp::Report(PlayerReport {
                    reporter: character,
                    reported,
                    reported_name: name,
                    reason,
                    category,
                    comment,
                    detail,
                    locale,
                })),
                Err(_) => metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1),
            },
            PlayerCall::Friend {
                request,
                character: other,
            } => match (FriendRequest::from_wire(request), self.known(&other)) {
                (Some(request), Some(other)) if other != character => {
                    self.friend(character, other, request);
                }
                _ => metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1),
            },
        }
    }

    /// The data of `character` as other players see it.
    pub(crate) fn player_view(&self, character: Uuid) -> Option<PlayerDataView> {
        let person = self.people.get(&character)?;
        Some(PlayerDataView {
            character: character.hyphenated().to_string().into_bytes(),
            name: person.name.as_bytes().to_vec(),
            pronoun: DEFAULT_PRONOUN,
            icon: person
                .settings
                .icon
                .as_ref()
                .map(icon_of_bytes)
                .unwrap_or_default(),
            guild: person
                .company
                .map_or(NO_GUILD, |(company, _)| *company.as_bytes()),
            level: person.settings.level,
            faction: person.settings.faction,
            world: self.rules.world_id.hyphenated().to_string().into_bytes(),
            presence: NO_PLATFORM_PRESENCE,
            platform: NO_PLATFORM,
            platform_user: Vec::new(),
        })
    }

    fn search(
        &mut self,
        caller: Uuid,
        header: FacetHeader,
        request: [u8; 16],
        name: &str,
        limit: u32,
    ) {
        let start = name.to_lowercase();
        let most = usize::try_from(u64::from(limit).min(MAX_REQUESTED_PLAYERS)).unwrap_or(0);
        let mut found: Vec<(&String, Uuid)> = self
            .names
            .iter()
            .filter(|(known, _)| !start.is_empty() && known.starts_with(&start))
            .map(|(known, character)| (known, *character))
            .collect();
        found.sort();
        let players: Vec<PlayerDataView> = found
            .into_iter()
            .take(most)
            .filter_map(|(_, character)| self.player_view(character))
            .collect();
        let push = if players.is_empty() {
            PlayerPush::RequestFailed(RequestFailedMsg {
                header,
                request,
                reason: FAILURE_NO_DATA,
            })
        } else {
            PlayerPush::SearchResults(PlayerDataResponseMsg {
                header,
                request,
                players,
            })
        };
        self.answer(caller, header, SocialPush::Player(push));
    }

    fn friend(&mut self, character: Uuid, other: Uuid, request: FriendRequest) {
        let invited_me = self
            .people
            .get(&character)
            .is_some_and(|person| person.invites_in.contains(&other));
        match request {
            FriendRequest::Invite | FriendRequest::Accept if invited_me => {
                self.befriend(character, other);
            }
            FriendRequest::Invite
                if !self.lets_in(other, self.privacy_of(other).friend_requests, character) =>
            {
                // Its privacy settings refuse the request; retail's answer to the sender is
                // Unknown, so it gets none (Inferred).
                metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            }
            FriendRequest::Invite => {
                let fresh = self.people.get_mut(&character).is_some_and(|person| {
                    !person.friends.contains(&other) && person.invites_out.insert(other)
                });
                if fresh {
                    if let Some(invitee) = self.people.get_mut(&other) {
                        invitee.invites_in.insert(character);
                    }
                    self.store(StoreOp::AddLink {
                        character,
                        other,
                        kind: LinkKind::Invite,
                    });
                }
            }
            FriendRequest::Accept => {
                metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
            }
            FriendRequest::Reject => self.drop_friend_invite(other, character),
            FriendRequest::Remove => {
                self.drop_friend_invite(character, other);
                let removed = self
                    .people
                    .get_mut(&character)
                    .is_some_and(|person| person.friends.remove(&other));
                if removed {
                    if let Some(friend) = self.people.get_mut(&other) {
                        friend.friends.remove(&character);
                    }
                    for (from, to) in [(character, other), (other, character)] {
                        self.store(StoreOp::RemoveLink {
                            character: from,
                            other: to,
                            kind: LinkKind::Friend,
                        });
                    }
                }
            }
        }
    }

    /// Ends the friend invite of `inviter` to `invitee`.
    fn drop_friend_invite(&mut self, inviter: Uuid, invitee: Uuid) {
        let removed = self
            .people
            .get_mut(&inviter)
            .is_some_and(|person| person.invites_out.remove(&invitee));
        if let Some(person) = self.people.get_mut(&invitee) {
            person.invites_in.remove(&inviter);
        }
        if removed {
            self.store(StoreOp::RemoveLink {
                character: inviter,
                other: invitee,
                kind: LinkKind::Invite,
            });
        }
    }

    /// `character` accepts the invite of `other`: they are friends, and each gets the other's
    /// online state.
    fn befriend(&mut self, character: Uuid, other: Uuid) {
        self.drop_friend_invite(other, character);
        self.drop_friend_invite(character, other);
        for (from, to) in [(character, other), (other, character)] {
            if let Some(person) = self.people.get_mut(&from) {
                person.friends.insert(to);
            }
            self.store(StoreOp::AddLink {
                character: from,
                other: to,
                kind: LinkKind::Friend,
            });
        }
        for (told, about) in [(character, other), (other, character)] {
            let state = if self.is_online(about) {
                ONLINE_STATE_ONLINE
            } else {
                ONLINE_STATE_OFFLINE
            };
            self.push(
                &[told],
                SocialPush::Player(PlayerPush::OnlineState {
                    character: about.hyphenated().to_string(),
                    state,
                }),
            );
        }
    }

    pub(crate) fn voice_call(
        &mut self,
        character: Uuid,
        header: FacetHeader,
        call: VoiceCall,
        now_ms: u64,
    ) {
        if let VoiceCall::ActivationState { active } = call {
            if let Some(person) = self.people.get_mut(&character) {
                person.settings.voice_active = active;
            }
            self.store_settings(character);
            return;
        }
        if let VoiceCall::ReportError { error } = call {
            tracing::info!(%character, error, "client voice service error");
            return;
        }
        let Some(service) = self.rules.voice.clone() else {
            metrics::counter!(METRIC_CALLS, RESULT_LABEL => NO_VOICE).increment(1);
            return;
        };
        let now_s = now_ms / MS_PER_SECOND;
        let push = match call {
            VoiceCall::Register => VoicePush::Registered {
                api_url: service.api_url.clone(),
                issuer: service.issuer.clone(),
                domain: service.domain_text(),
                flag: REGISTERED_FLAG,
            },
            VoiceCall::LoginToken { request } => VoicePush::Token {
                token: service.token(TokenAction::Login, character, request, None, now_s),
                kind: TOKEN_LOGIN,
                request,
            },
            VoiceCall::JoinToken {
                request,
                channel,
                kind,
            } => {
                let target = if kind == CHANNEL_POSITIONAL {
                    service.positional_uri(&channel)
                } else {
                    service.group_uri(&channel)
                };
                if !self.may_hear(&service, character, &target) {
                    metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
                    return;
                }
                VoicePush::Token {
                    token: service.token(
                        TokenAction::Join,
                        character,
                        request,
                        Some(&target),
                        now_s,
                    ),
                    kind: TOKEN_JOIN,
                    request,
                }
            }
            VoiceCall::TranscriptionToken {
                request,
                channel_uri,
            } => {
                if !self.may_hear(&service, character, &channel_uri) {
                    metrics::counter!(METRIC_CALLS, RESULT_LABEL => REFUSED).increment(1);
                    return;
                }
                VoicePush::Token {
                    token: service.token(
                        TokenAction::Transcription,
                        character,
                        request,
                        Some(&channel_uri),
                        now_s,
                    ),
                    kind: TOKEN_TRANSCRIPTION,
                    request,
                }
            }
            VoiceCall::WorldChannels => {
                let Some(position) = self
                    .people
                    .get(&character)
                    .and_then(|person| person.online.as_ref())
                    .map(|online| online.position)
                else {
                    return;
                };
                let channel = service.positional_channel(position);
                let uri = service.positional_uri(&channel);
                VoicePush::WorldChannels {
                    channels: vec![(channel, uri)],
                }
            }
            VoiceCall::GroupChannel => {
                let Some(group) = self
                    .people
                    .get(&character)
                    .and_then(|person| person.group)
                    .and_then(|group| self.groups.get(&group))
                else {
                    return;
                };
                VoicePush::GroupChannel {
                    uri: service.group_uri(&group.voice_channel),
                    channel: group.voice_channel.clone(),
                }
            }
            VoiceCall::ActivationState { .. } | VoiceCall::ReportError { .. } => return,
        };
        self.answer(character, header, SocialPush::Voice(push));
    }
}

impl SocialWorld {
    /// Whether `character` may get a token for the channel at address `uri`: its group's channel,
    /// or a positional channel at or next to its place, as its voice privacy setting allows
    /// ([`crate::privacy`]). Any other channel is refused with no
    /// answer (Inferred: retail's refusal of a token is Unknown; no token is the safe answer).
    fn may_hear(&self, service: &VoiceService, character: Uuid, uri: &str) -> bool {
        let Some(person) = self.people.get(&character) else {
            return false;
        };
        let voice = self.privacy_of(character).voice;
        if voice == Audience::NoOne {
            return false;
        }
        let in_group = person
            .group
            .and_then(|group| self.groups.get(&group))
            .is_some_and(|group| service.group_uri(&group.voice_channel) == uri);
        let near = voice == Audience::Everyone
            && person.online.as_ref().is_some_and(|online| {
                service
                    .positional_channels_near(online.position)
                    .iter()
                    .any(|channel| service.positional_uri(channel) == uri)
            });
        in_group || near
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::social::players::{
        FRIEND_STATUS_CHANGE_TYPE_INDEX, ONLINE_STATE_CHANGE_TYPE_INDEX,
        PLAYER_DATA_RESPONSE_TYPE_INDEX, PLAYER_PHASE_ID_TYPE_INDEX, REPORT_PLAYER_TYPE_INDEX,
        REQUEST_FAILED_TYPE_INDEX, SEARCH_PLAYERS_TYPE_INDEX,
    };
    use nw_messages::social::voice::{
        CHANNEL_GROUP, JOIN_TOKEN_TYPE_INDEX, REGISTER_TYPE_INDEX, REGISTERED_TYPE_INDEX,
        TOKEN_TYPE_INDEX, TRANSCRIPTION_TOKEN_TYPE_INDEX, WORLD_CHANNEL_URIS_TYPE_INDEX,
        WORLD_CHANNELS_TYPE_INDEX,
    };

    use super::*;
    use crate::Effect;
    use crate::voice::VoiceService;
    use crate::world::tests::{NOW_MS, call, character, join, pushes, sim, text, world};

    const REQUEST: [u8; 16] = [9; 16];

    #[test]
    fn a_search_finds_names_by_their_start_or_fails_as_retail() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        let mut args = REQUEST.to_vec();
        args.extend(text("b"));
        args.extend(25_u32.to_be_bytes());
        let effects = call(
            &mut world,
            1,
            "sim-a",
            SEARCH_PLAYERS_TYPE_INDEX,
            &args,
            NOW_MS,
        );
        let [(to, SocialPush::Player(PlayerPush::SearchResults(results)))]: [_; 1] =
            pushes(&effects, PLAYER_DATA_RESPONSE_TYPE_INDEX)
                .try_into()
                .expect("one answer")
        else {
            panic!("search results");
        };
        assert_eq!(to, [character(1)]);
        assert_eq!(results.players.len(), 1);
        assert_eq!(results.players[0].name, b"Bob");
        let mut args = REQUEST.to_vec();
        args.extend(text("zz"));
        args.extend(25_u32.to_be_bytes());
        let effects = call(
            &mut world,
            1,
            "sim-a",
            SEARCH_PLAYERS_TYPE_INDEX,
            &args,
            NOW_MS,
        );
        assert_eq!(pushes(&effects, REQUEST_FAILED_TYPE_INDEX).len(), 1);
        let Effect::Deliver { delivery, .. } = &effects[0] else {
            panic!("a delivery");
        };
        assert!(matches!(
            delivery.header,
            nw_bus::SocialHeader::Echo {
                target: 11,
                entity: 22
            }
        ));
    }

    #[test]
    fn an_invite_and_its_acceptance_make_friends() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "sim-a");
        let friend = |request: u32, other: u128| {
            let mut args = request.to_be_bytes().to_vec();
            args.extend(text(&character(other).hyphenated().to_string()));
            args
        };
        call(
            &mut world,
            1,
            "sim-a",
            FRIEND_STATUS_CHANGE_TYPE_INDEX,
            &friend(1, 2),
            NOW_MS,
        );
        assert!(
            world.people[&character(2)]
                .invites_in
                .contains(&character(1))
        );
        let effects = call(
            &mut world,
            2,
            "sim-a",
            FRIEND_STATUS_CHANGE_TYPE_INDEX,
            &friend(2, 1),
            NOW_MS,
        );
        assert!(world.people[&character(1)].friends.contains(&character(2)));
        assert!(world.people[&character(2)].friends.contains(&character(1)));
        assert_eq!(pushes(&effects, ONLINE_STATE_CHANGE_TYPE_INDEX).len(), 2);
        call(
            &mut world,
            1,
            "sim-a",
            FRIEND_STATUS_CHANGE_TYPE_INDEX,
            &friend(4, 2),
            NOW_MS,
        );
        assert!(world.people[&character(2)].friends.is_empty());
    }

    #[test]
    fn a_player_on_an_instance_sim_is_in_its_phase() {
        const PHASE: u64 = 9;
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        join(&mut world, 2, "Bob", "inst-1");
        let mut args = REQUEST.to_vec();
        args.extend(text(&character(2).hyphenated().to_string()));
        let ask = |world: &mut SocialWorld, type_index, answer| {
            let effects = call(world, 1, "sim-a", type_index, &args, NOW_MS);
            pushes(&effects, answer)[0].1.clone()
        };
        let phase_id = |world: &mut SocialWorld| {
            ask(
                world,
                PLAYER_PHASE_ID_TYPE_INDEX,
                nw_messages::social::players::PHASE_ID_RESPONSE_TYPE_INDEX,
            )
        };
        let id_push = |phase| {
            SocialPush::Player(PlayerPush::PhaseId {
                character: CharacterRef(Some(*character(2).as_bytes())),
                phase,
                trailer: PHASE_ID_TRAILER,
            })
        };
        assert_eq!(phase_id(&mut world), id_push(OPEN_WORLD_PHASE));
        world.set_sim_phase(&sim("inst-1"), Some(PHASE));
        assert_eq!(phase_id(&mut world), id_push(PHASE));
        let SocialPush::Player(PlayerPush::PhaseData { sector, .. }) = ask(
            &mut world,
            nw_messages::social::players::PLAYER_PHASE_DATA_TYPE_INDEX,
            nw_messages::social::players::PHASE_DATA_RESPONSE_TYPE_INDEX,
        ) else {
            panic!("phase data");
        };
        assert_eq!(sector, PHASE);
        world.sim_lost(&sim("inst-1"), NOW_MS);
        assert_eq!(phase_id(&mut world), id_push(OPEN_WORLD_PHASE));
    }

    #[test]
    fn phase_and_report_calls_are_answered_and_stored() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        let other = character(2).hyphenated().to_string();
        let mut args = REQUEST.to_vec();
        args.extend(text(&other));
        let effects = call(
            &mut world,
            1,
            "sim-a",
            PLAYER_PHASE_ID_TYPE_INDEX,
            &args,
            NOW_MS,
        );
        assert_eq!(
            pushes(
                &effects,
                nw_messages::social::players::PHASE_ID_RESPONSE_TYPE_INDEX
            )
            .len(),
            1
        );
        let mut args = 4_u32.to_be_bytes().to_vec();
        args.extend(text(&other));
        args.extend(text(""));
        args.extend(text("Bob"));
        args.extend(2_u32.to_be_bytes());
        args.extend(text(""));
        args.extend(text("en-US"));
        let effects = call(
            &mut world,
            1,
            "sim-a",
            REPORT_PLAYER_TYPE_INDEX,
            &args,
            NOW_MS,
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Store(StoreOp::Report(_))))
        );
    }

    #[test]
    fn voice_calls_get_the_retail_answers_with_a_voice_service() {
        let mut world = world();
        join(&mut world, 1, "Ann", "sim-a");
        assert!(call(&mut world, 1, "sim-a", REGISTER_TYPE_INDEX, &[], NOW_MS).is_empty());
        world.rules.voice = Some(VoiceService {
            api_url: "https://voice.example/api2/".to_owned(),
            issuer: "issuer".to_owned(),
            domain: "voice.example".to_owned(),
            key: b"key".to_vec(),
            channel_suffix: "S".to_owned(),
            cell_size_m: 512.0,
        });
        let effects = call(&mut world, 1, "sim-a", REGISTER_TYPE_INDEX, &[], NOW_MS);
        assert_eq!(pushes(&effects, REGISTERED_TYPE_INDEX).len(), 1);
        let effects = call(
            &mut world,
            1,
            "sim-a",
            WORLD_CHANNELS_TYPE_INDEX,
            &[],
            NOW_MS,
        );
        let [(_, SocialPush::Voice(VoicePush::WorldChannels { channels }))]: [_; 1] =
            pushes(&effects, WORLD_CHANNEL_URIS_TYPE_INDEX)
                .try_into()
                .expect("one answer")
        else {
            panic!("channels");
        };
        assert_eq!(channels[0].0, "000000_0");
        let mut args = 2_u16.to_be_bytes().to_vec();
        args.extend(text(&channels[0].0));
        args.extend(CHANNEL_POSITIONAL.to_be_bytes());
        let effects = call(&mut world, 1, "sim-a", JOIN_TOKEN_TYPE_INDEX, &args, NOW_MS);
        let [(_, SocialPush::Voice(VoicePush::Token { kind, request, .. }))]: [_; 1] =
            pushes(&effects, TOKEN_TYPE_INDEX)
                .try_into()
                .expect("one token")
        else {
            panic!("a token");
        };
        assert_eq!((kind, request), (TOKEN_JOIN, 2));
        let token_for = |world: &mut SocialWorld, channel: &str, kind: u32| {
            let mut args = 3_u16.to_be_bytes().to_vec();
            args.extend(text(channel));
            args.extend(kind.to_be_bytes());
            let effects = call(world, 1, "sim-a", JOIN_TOKEN_TYPE_INDEX, &args, NOW_MS);
            pushes(&effects, TOKEN_TYPE_INDEX).len()
        };
        assert_eq!(
            token_for(&mut world, "009009_0", CHANNEL_POSITIONAL),
            0,
            "far away"
        );
        assert_eq!(
            token_for(&mut world, "000001_0", CHANNEL_POSITIONAL),
            1,
            "next place"
        );
        let group_kind = CHANNEL_GROUP;
        assert_eq!(
            token_for(&mut world, "someone-elses", group_kind),
            0,
            "not its group"
        );
        let group = nw_db::SocialGroup {
            id: Uuid::from_u128(900),
            leader: character(1),
            voice_channel: "own-group".to_owned(),
            members: vec![character(1)],
        };
        world.people.get_mut(&character(1)).expect("Ann").group = Some(group.id);
        world.groups.insert(group.id, group);
        assert_eq!(
            token_for(&mut world, "own-group", group_kind),
            1,
            "its group"
        );
        let service = world.rules.voice.clone().expect("voice");
        let transcribe = |world: &mut SocialWorld, uri: &str| {
            let mut args = 4_u16.to_be_bytes().to_vec();
            args.extend(text(uri));
            let effects = call(
                world,
                1,
                "sim-a",
                TRANSCRIPTION_TOKEN_TYPE_INDEX,
                &args,
                NOW_MS,
            );
            pushes(&effects, TOKEN_TYPE_INDEX).len()
        };
        assert_eq!(transcribe(&mut world, &service.group_uri("own-group")), 1);
        assert_eq!(transcribe(&mut world, &service.group_uri("other")), 0);
    }
}
