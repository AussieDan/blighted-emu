// SPDX-License-Identifier: AGPL-3.0-or-later
//! The audio of the voice rooms: the call setup's SDP answer and a UDP relay.
//!
//! Observed (static analysis of `Bin64/vivoxsdk.dll`, notes
//! `local-data/findings/2026-11-07-vivox-invite-chan-add-re.md`):
//! - After its room join, the SDK sends `invite` (an `iq` set to the room, `urn:vivox:muc`) with
//!   a fixed SDP offer: `10.0.0.11` port 0, payload type 127 `VIVOXVANI/32000`. It sends RTP to
//!   the address of the answer's first `m=` line and that line's port, over plain UDP
//!   (uvgRTP). A later room of the same call is a `chan_add` with no SDP: one stream carries
//!   every room of a player.
//! - Every 5 s it sends a 28-byte ping: `80 7E <seq16> 00000000 <ssrc> "lmpp" <sec> <frac>
//!   <counter>`. A ping without the marker bit is a request, and the answer is the same packet
//!   with the marker bit set.
//!
//! The relay gives each call its own UDP port: a packet that arrives there is that player's.
//! It answers ping requests and sends the payload of every other packet (the speaker's VANI
//! blocks, which name the speaker and the room: [`crate::vani`]) to each other player that
//! shares a room with the sender, in that receiver's own RTP stream (its SSRC, sequence and
//! timestamp: the SDK queues packets by timestamp). Each player gets the roster of its rooms
//! when its audio starts, when a room's members change and every [`ROSTER_INTERVAL_S`]: the SDK
//! learns who a `userHash` is only from it, and the retail server sent the client a few packets
//! at the start and about one every 15 s besides the ping answers (32 retail call reports of
//! `captures/`: packets received = pings answered + 3 to 8). With `echo`, the sender also gets
//! its own audio back, so one player can test the path alone.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use crate::vani::{Member, OutStream, hash, payload, roster_payloads, sip_address};

/// The payload type and clock of the SDK's audio (its offer's `a=rtpmap`).
const AUDIO_PAYLOAD_TYPE: u8 = 127;
const AUDIO_RTPMAP: &str = "VIVOXVANI/32000";
/// The SDK's ping: payload type 126, 28 bytes, `lmpp` at byte 12.
const PING_PAYLOAD_TYPE: u8 = 0x7e;
const PING_LEN: usize = 28;
const PING_TAG: &[u8; 4] = b"lmpp";
const PING_TAG_AT: usize = 12;
/// The RTP marker bit in byte 1, and the payload type bits.
const MARKER_BIT: u8 = 0x80;
const PAYLOAD_TYPE_BITS: u8 = 0x7f;
/// Byte 1 of an RTP header: marker and payload type.
const MARKER_BYTE: usize = 1;
/// How often each player gets the roster of its rooms, seconds (Inferred from the retail call
/// reports: about one server packet every 15 s besides the ping answers).
const ROSTER_INTERVAL_S: u64 = 15;
/// How often the relay logs each call's packet counts, seconds.
const STATS_INTERVAL_S: u64 = 10;
/// How many of a call's first audio packets go in the log as hex, and how many bytes of each.
const LOGGED_PACKETS: u32 = 3;
const LOGGED_BYTES: usize = 48;
/// Largest UDP datagram the relay reads.
const DATAGRAM_BYTES: usize = 2_048;
/// The SDP line prefix of the offer's codec parameters, copied into the answer.
const FMTP_PREFIX: &str = "a=fmtp:";
/// The start of a positional channel's room (`confctl-d-`).
const POSITIONAL_ROOM_PREFIX: &str = "confctl-d-";
/// The channel properties of a positional room (Inferred values: the Vivox SDK's documented 3D
/// defaults, audible distance 32, conversational distance 1, rolloff 1.0 with the
/// inverse-distance model 1; retail's values are not in any capture).
const POSITIONAL_TYPE: &str = "positional";
const THRESHOLD_DISTANCE: u32 = 32;
const CLAMPING_DISTANCE: u32 = 1;
const DISTANCE_MODEL: u8 = 1;
const ROLLOFF: f32 = 1.0;
/// SDP line end.
const CRLF: &str = "\r\n";

/// `[media]` of `config/voice-server.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    /// The address the answer names: where the game sends its audio.
    pub address: IpAddr,
    /// The address the relay's UDP ports listen on.
    pub bind: IpAddr,
    /// The first and last UDP port of the calls.
    pub port_min: u16,
    /// The last UDP port of the calls.
    pub port_max: u16,
    /// A player also hears itself (a test with one player).
    #[serde(default)]
    pub echo: bool,
}

/// The SDP answer to an invite: the relay's address and the call's `port`, the offer's codec
/// parameters, and the 3D properties of a positional room.
pub fn answer_sdp(address: IpAddr, port: u16, offer: &str, room: &str) -> String {
    let mut lines = vec![
        "v=0".to_owned(),
        format!("o=vivox 0 0 IN IP4 {address}"),
        "s= ".to_owned(),
        format!("c=IN IP4 {address}"),
        "t=0 0".to_owned(),
        format!("m=audio {port} RTP/AVP {AUDIO_PAYLOAD_TYPE}"),
        format!("a=rtpmap:{AUDIO_PAYLOAD_TYPE} {AUDIO_RTPMAP}"),
    ];
    lines.extend(
        offer
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with(FMTP_PREFIX))
            .map(str::to_owned),
    );
    lines.push("a=vx_rtcp:0".to_owned());
    if is_positional(room) {
        lines.extend([
            format!("a=vx_chan_type:{POSITIONAL_TYPE}"),
            format!("a=vx_threshold_distance:{THRESHOLD_DISTANCE}"),
            format!("a=vx_clamping_distance:{CLAMPING_DISTANCE}"),
            format!("a=vx_rolloff:{DISTANCE_MODEL} {ROLLOFF:.1}"),
        ]);
    }
    let mut sdp = lines.join(CRLF);
    sdp.push_str(CRLF);
    sdp
}

/// The `vx_chn_properties` element of a positional room, for a `chan_add` answer.
pub fn positional_properties_xml() -> String {
    format!(
        "<vx_chn_properties><vx_chan_type>{POSITIONAL_TYPE}</vx_chan_type>\
         <vx_threshold_distance>{THRESHOLD_DISTANCE}</vx_threshold_distance>\
         <vx_clamping_distance>{CLAMPING_DISTANCE}</vx_clamping_distance>\
         <vx_distance_model>{DISTANCE_MODEL}</vx_distance_model>\
         <vx_rolloff>{ROLLOFF:.1}</vx_rolloff></vx_chn_properties>"
    )
}

/// Whether `room` (`<channel>@muc.<realm>`) is a positional channel.
pub fn is_positional(room: &str) -> bool {
    room.starts_with(POSITIONAL_ROOM_PREFIX)
}

/// Whether `packet` is a ping request of the SDK.
pub fn is_ping_request(packet: &[u8]) -> bool {
    packet.len() == PING_LEN
        && packet[MARKER_BYTE] == PING_PAYLOAD_TYPE
        && &packet[PING_TAG_AT..PING_TAG_AT + PING_TAG.len()] == PING_TAG
}

/// The answer to the ping request `packet`: the same packet with the marker bit set.
pub fn ping_reply(packet: &[u8]) -> Vec<u8> {
    let mut reply = packet.to_vec();
    reply[MARKER_BYTE] |= MARKER_BIT;
    reply
}

/// What one call's port saw, for the log.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CallStats {
    audio_in: u32,
    pings_in: u32,
    other_in: u32,
    audio_out: u32,
}

/// Whether `packet` is a ping (request or answer), which the relay never forwards.
fn is_ping(packet: &[u8]) -> bool {
    packet.len() == PING_LEN && packet[MARKER_BYTE] & PAYLOAD_TYPE_BITS == PING_PAYLOAD_TYPE
}

/// One player's call: its UDP port, the address its audio comes from, its rooms with their
/// `sessionHash`, its own address and `userHash`, and its outgoing stream.
#[derive(Debug)]
struct Call {
    socket: Arc<UdpSocket>,
    port: u16,
    peer: Option<SocketAddr>,
    rooms: HashMap<String, u32>,
    address: String,
    user_hash: u32,
    out: OutStream,
    reader: JoinHandle<()>,
}

/// A packet to send: the receiving call's socket, its address and the bytes.
type Outgoing = (Arc<UdpSocket>, SocketAddr, Vec<u8>);

/// The calls by the full address of their player.
#[derive(Debug, Default)]
struct Calls {
    by_jid: HashMap<String, Call>,
}

impl Calls {
    /// The payload `payload` of `jid` to each other player with a known address that shares a
    /// room with it, and to itself with `echo`, each in its own stream.
    fn forward(&mut self, jid: &str, payload: &[u8], echo: bool) -> Vec<Outgoing> {
        let Some(rooms) = self.by_jid.get(jid).map(|call| call.rooms.clone()) else {
            return Vec::new();
        };
        self.by_jid
            .iter_mut()
            .filter(|(other, call)| {
                if other.as_str() == jid {
                    echo
                } else {
                    call.rooms.keys().any(|room| rooms.contains_key(room))
                }
            })
            .filter_map(|(_, call)| {
                let peer = call.peer?;
                Some((Arc::clone(&call.socket), peer, call.out.packet(payload)))
            })
            .collect()
    }

    /// The roster of every room of every call with a known address, each in its own stream.
    fn rosters(&mut self) -> Vec<Outgoing> {
        let members: Vec<(String, Member)> = self
            .by_jid
            .values()
            .flat_map(|call| {
                call.rooms.iter().map(|(room, session_hash)| {
                    (
                        room.clone(),
                        Member {
                            user_hash: call.user_hash,
                            session_hash: *session_hash,
                            address: call.address.clone(),
                        },
                    )
                })
            })
            .collect();
        let mut outgoing = Vec::new();
        for call in self.by_jid.values_mut() {
            let Some(peer) = call.peer else {
                continue;
            };
            let own: Vec<Member> = members
                .iter()
                .filter(|(room, _)| call.rooms.contains_key(room))
                .map(|(_, member)| member.clone())
                .collect();
            for payload in roster_payloads(&own) {
                outgoing.push((Arc::clone(&call.socket), peer, call.out.packet(&payload)));
            }
        }
        outgoing
    }
}

/// Sends `outgoing`; a lost datagram is lost audio, as on any UDP voice path. How many went.
async fn send_all(outgoing: Vec<Outgoing>) -> u32 {
    let mut sent = 0;
    for (socket, to, packet) in outgoing {
        if socket.send_to(&packet, to).await.is_ok() {
            sent += 1;
        }
    }
    sent
}

/// The relay of every call.
#[derive(Debug, Clone)]
pub struct Relay {
    config: Arc<MediaConfig>,
    realm: Arc<str>,
    calls: Arc<Mutex<Calls>>,
}

impl Relay {
    /// A relay of `config` for the SIP realm `realm`, with no calls.
    pub fn new(config: MediaConfig, realm: &str) -> Self {
        Self {
            config: Arc::new(config),
            realm: realm.into(),
            calls: Arc::default(),
        }
    }

    /// The address the answers name.
    pub fn address(&self) -> IpAddr {
        self.config.address
    }

    /// The `sessionHash` of `room` (`<channel>@muc.<realm>`).
    fn session_hash(&self, room: &str) -> u32 {
        let channel = room.split('@').next().unwrap_or(room);
        hash(sip_address(channel, &self.realm).as_bytes())
    }

    /// The call of `jid` with `room`: a new UDP port for a new call, the same one when the call
    /// exists (an invite again after a failed `chan_add`). Its port.
    ///
    /// # Errors
    ///
    /// Returns an error when no port of the range is free.
    pub async fn open_call(&self, jid: &str, room: &str) -> io::Result<u16> {
        let session_hash = self.session_hash(room);
        if let Some(port) = self.with_calls(|calls| {
            let call = calls.by_jid.get_mut(jid)?;
            call.rooms.insert(room.to_owned(), session_hash);
            Some(call.port)
        }) {
            self.refresh_rosters().await;
            return Ok(port);
        }
        let socket = Arc::new(self.bind_free_port().await?);
        let port = socket.local_addr()?.port();
        let user = jid.split('@').next().unwrap_or(jid);
        let address = sip_address(user, &self.realm);
        let user_hash = hash(address.as_bytes());
        let reader = tokio::spawn(read_call(self.clone(), jid.to_owned(), Arc::clone(&socket)));
        self.with_calls(|calls| {
            calls.by_jid.insert(
                jid.to_owned(),
                Call {
                    socket,
                    port,
                    peer: None,
                    rooms: HashMap::from([(room.to_owned(), session_hash)]),
                    address,
                    user_hash,
                    out: OutStream::new(user_hash.rotate_left(16) ^ u32::from(port)),
                    reader,
                },
            );
        });
        Ok(port)
    }

    /// `room` joins the call of `jid`; `false` when it has no call.
    pub async fn add_room(&self, jid: &str, room: &str) -> bool {
        let session_hash = self.session_hash(room);
        let added = self.with_calls(|calls| {
            calls
                .by_jid
                .get_mut(jid)
                .map(|call| call.rooms.insert(room.to_owned(), session_hash))
                .is_some()
        });
        if added {
            self.refresh_rosters().await;
        }
        added
    }

    /// `room` leaves the call of `jid`.
    pub async fn leave_room(&self, jid: &str, room: &str) {
        self.with_calls(|calls| {
            if let Some(call) = calls.by_jid.get_mut(jid) {
                call.rooms.remove(room);
            }
        });
        self.refresh_rosters().await;
    }

    /// The call of `jid` ends: its port closes, and the others get rosters without it.
    pub fn close_call(&self, jid: &str) {
        if let Some(call) = self.with_calls(|calls| calls.by_jid.remove(jid)) {
            call.reader.abort();
            let relay = self.clone();
            tokio::spawn(async move { relay.refresh_rosters().await });
        }
    }

    /// Every player gets the roster of its rooms.
    pub async fn refresh_rosters(&self) {
        let outgoing = self.with_calls(Calls::rosters);
        send_all(outgoing).await;
    }

    /// Sends every player the roster of its rooms every [`ROSTER_INTERVAL_S`], until the process
    /// stops.
    pub async fn run_rosters(self) {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(ROSTER_INTERVAL_S));
        loop {
            ticker.tick().await;
            self.refresh_rosters().await;
        }
    }

    fn with_calls<T>(&self, change: impl FnOnce(&mut Calls) -> T) -> T {
        change(&mut self.calls.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// A UDP socket on the first free port of the range.
    async fn bind_free_port(&self) -> io::Result<UdpSocket> {
        let mut last = io::Error::new(io::ErrorKind::AddrInUse, "no free voice port");
        for port in self.config.port_min..=self.config.port_max {
            match UdpSocket::bind((self.config.bind, port)).await {
                Ok(socket) => return Ok(socket),
                Err(error) => last = error,
            }
        }
        Err(last)
    }
}

/// Reads the call of `jid` until it closes: pings answered, the first packet answered with the
/// rosters, audio forwarded, its counts logged every [`STATS_INTERVAL_S`] and its first audio
/// packets logged as hex.
async fn read_call(relay: Relay, jid: String, socket: Arc<UdpSocket>) {
    let mut buffer = vec![0; DATAGRAM_BYTES];
    let mut stats = CallStats::default();
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(STATS_INTERVAL_S));
    loop {
        let received = tokio::select! {
            received = socket.recv_from(&mut buffer) => received,
            _ = ticker.tick() => {
                if stats != CallStats::default() {
                    tracing::info!(%jid, ?stats, "voice call packets");
                }
                continue;
            }
        };
        let Ok((read, from)) = received else {
            return;
        };
        let packet = &buffer[..read];
        let first = relay.with_calls(|calls| {
            let call = calls.by_jid.get_mut(&jid)?;
            Some(call.peer.replace(from).is_none())
        });
        if first == Some(true) {
            tracing::info!(%jid, %from, "voice audio started");
            relay.refresh_rosters().await;
        }
        if is_ping_request(packet) {
            stats.pings_in += 1;
            let _ = socket.send_to(&ping_reply(packet), from).await;
            continue;
        }
        if is_ping(packet) {
            stats.pings_in += 1;
            continue;
        }
        let Some(payload) = payload(packet)
            .filter(|_| packet[MARKER_BYTE] & PAYLOAD_TYPE_BITS == AUDIO_PAYLOAD_TYPE)
        else {
            stats.other_in += 1;
            continue;
        };
        stats.audio_in += 1;
        if stats.audio_in <= LOGGED_PACKETS {
            let shown: String = packet
                .iter()
                .take(LOGGED_BYTES)
                .map(|byte| format!("{byte:02x}"))
                .collect();
            tracing::info!(%jid, bytes = read, hex = %shown, "voice packet");
        }
        let echo = relay.config.echo;
        let outgoing = relay.with_calls(|calls| calls.forward(&jid, payload, echo));
        stats.audio_out += send_all(outgoing).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELAY: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    /// The SDK's offer as `vivoxsdk.dll` writes it (codec mask and jitter value made up).
    const OFFER: &str = "v=0\r\no=vivox 0 0 IN IP4 10.0.0.11\r\ns= \r\nc=IN IP4 10.0.0.11\r\n\
        t=0 0\r\nm=audio 0 RTP/AVP 127\r\na=rtpmap:127 VIVOXVANI/32000\r\n\
        a=fmtp:127 ver=3;mode=3;sub-types=1,7;codecs=0x1f\r\na=vx_jc:3\r\na=vx_rtcp:0\r\n\
        a=vx_ext:0x4\r\n";

    #[test]
    fn the_answer_names_the_relay_and_keeps_the_codecs() {
        let positional = answer_sdp(RELAY, 40_002, OFFER, "confctl-d-issuer.018012_0@muc.x");
        assert!(positional.starts_with("v=0\r\no=vivox 0 0 IN IP4 127.0.0.1\r\n"));
        assert!(positional.contains("c=IN IP4 127.0.0.1\r\n"));
        assert!(positional.contains("m=audio 40002 RTP/AVP 127\r\n"));
        assert!(positional.contains("a=fmtp:127 ver=3;mode=3;sub-types=1,7;codecs=0x1f\r\n"));
        assert!(positional.contains("a=vx_chan_type:positional\r\n"));
        let group = answer_sdp(RELAY, 40_002, OFFER, "confctl-g-issuer.abc@muc.x");
        assert!(!group.contains("vx_chan_type"));
        assert!(group.ends_with("a=vx_rtcp:0\r\n"));
    }

    #[test]
    fn a_ping_request_gets_its_marked_copy_and_audio_is_no_ping() {
        let mut ping = [0_u8; PING_LEN];
        ping[0] = 0x80;
        ping[MARKER_BYTE] = PING_PAYLOAD_TYPE;
        ping[PING_TAG_AT..PING_TAG_AT + 4].copy_from_slice(PING_TAG);
        assert!(is_ping_request(&ping));
        let reply = ping_reply(&ping);
        assert_eq!(reply[MARKER_BYTE], 0xfe);
        assert!(!is_ping_request(&reply) && is_ping(&reply));
        let audio = [0x80, AUDIO_PAYLOAD_TYPE, 0, 1, 0, 0, 0, 0, 0, 0, 0, 7, 9, 9];
        assert!(!is_ping_request(&audio) && !is_ping(&audio));
    }

    /// The next packet `socket` gets, with its sender's port; none within a second fails.
    async fn next(socket: &UdpSocket) -> (Vec<u8>, u16) {
        let mut buffer = [0_u8; DATAGRAM_BYTES];
        let (read, from) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            socket.recv_from(&mut buffer),
        )
        .await
        .expect("a packet in time")
        .expect("received");
        (buffer[..read].to_vec(), from.port())
    }

    #[tokio::test]
    async fn the_room_gets_rosters_pings_and_each_others_audio() {
        const REALM: &str = "game.example.org";
        let relay = Relay::new(
            MediaConfig {
                address: RELAY,
                bind: RELAY,
                port_min: 0,
                port_max: 0,
                echo: false,
            },
            REALM,
        );
        let room = format!("confctl-d-issuer.018012_0@muc.{REALM}");
        let ann_jid = format!("ann@{REALM}/voice");
        let bob_jid = format!("bob@{REALM}/voice");
        let ann_port = relay.open_call(&ann_jid, &room).await.expect("ann's call");
        let bob_port = relay.open_call(&bob_jid, &room).await.expect("bob's call");
        let ann = UdpSocket::bind((RELAY, 0)).await.expect("ann");
        let bob = UdpSocket::bind((RELAY, 0)).await.expect("bob");
        let mut ping = [0_u8; PING_LEN];
        ping[0] = 0x80;
        ping[MARKER_BYTE] = PING_PAYLOAD_TYPE;
        ping[PING_TAG_AT..PING_TAG_AT + 4].copy_from_slice(PING_TAG);
        bob.send_to(&ping, (RELAY, bob_port))
            .await
            .expect("bob's ping");
        let (roster, from) = next(&bob).await;
        assert_eq!(from, bob_port, "from bob's own call port");
        let roster = payload(&roster).expect("an RTP packet");
        let names = |payload: &[u8], name: &str| {
            payload
                .windows(name.len())
                .any(|window| window == name.as_bytes())
        };
        assert!(
            names(roster, &format!("sip:ann@{REALM}"))
                && names(roster, &format!("sip:bob@{REALM}"))
        );
        let (answer, _) = next(&bob).await;
        assert_eq!(answer, ping_reply(&ping));
        let audio = [
            0x80,
            AUDIO_PAYLOAD_TYPE,
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            7,
            1,
            2,
            3,
        ];
        ann.send_to(&audio, (RELAY, ann_port))
            .await
            .expect("ann's audio");
        let (_, _) = next(&ann).await;
        let (_, _) = next(&bob).await;
        let (heard, _) = next(&bob).await;
        assert_eq!(
            payload(&heard),
            Some(&[1_u8, 2, 3][..]),
            "ann's payload unchanged"
        );
        assert_ne!(&heard[8..12], &audio[8..12], "in bob's own stream");
        relay.close_call(&ann_jid);
        relay.close_call(&bob_jid);
    }
}
