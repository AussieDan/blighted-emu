// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SDK's audio payload (`VIVOXVANI`) as the relay needs it: the ids of speakers and rooms,
//! the in-band roster, and an RTP header per receiver.
//!
//! Observed (static analysis of `Bin64/vivoxsdk.dll`, notes
//! `local-data/findings/2026-11-07-vivox-vani-receive-re.md`; the hashes checked against live
//! packets of 2026-11-07):
//! - A payload is a 2-byte header (`60 01`: version 3, mode 1) and blocks. A block is a 2-byte
//!   header (length in bytes with this header: `byte0 * 4 + (byte1 >> 6)`; type: `byte1 & 0x3f`,
//!   9 = Opus) and four big-endian words: the speaker's `userHash`, the room's `sessionHash`, the
//!   speech energy and flags; the audio follows.
//! - `userHash` is Jenkins lookup3 `hashlittle` (seed 0) of the speaker's SIP address
//!   `sip:<user>@<realm>`; `sessionHash` of the room's `sip:<channel>@<realm>`. The hashes of
//!   a live player's packets (2026-11-07) matched both. The SDK hashes the address of the jid
//!   the server binds: with the account jid `.<issuer>.<id>+B.@<realm>` (2026-11-08) its
//!   packets carried the hash of `sip:.<issuer>.<id>+B.@<realm>`.
//! - The client learns which `userHash` is who only from roster blocks (flags `0x40`): entries
//!   of `len`, `eflags` (bit 0x1: in audio, needed to add the participant), `sessionHash`,
//!   `userHash` and the NUL-terminated SIP address, `len` = the bytes after the entry's 16.
//! - Its receive queue orders packets by RTP timestamp and keeps one per timestamp; any packet of
//!   a payload type other than 126 resets its RTP timeout.

use std::time::Instant;

/// The payload header: version 3, mode 1.
const PAYLOAD_HEADER: [u8; 2] = [0x60, 0x01];
/// The block type of the roster's block (Opus; any allowed type carries a roster).
const ROSTER_BLOCK_TYPE: u8 = 9;
/// The block flags of a roster.
const ROSTER_FLAGS: u32 = 0x40;
/// A roster entry's flag of a participant in the audio.
const IN_AUDIO: u32 = 0x1;
/// The bytes of a block header, of the four words after it, and of an entry's fixed part.
const BLOCK_HEADER_LEN: usize = 2;
const BLOCK_WORDS_LEN: usize = 16;
const ENTRY_HEADER_LEN: usize = 16;
/// The longest block: a 10-bit length.
const MAX_BLOCK_LEN: usize = 1_023;
/// The RTP header: version 2 with no padding, extension or CSRC; the audio payload type.
const RTP_FIRST_BYTE: u8 = 0x80;
const AUDIO_PAYLOAD_TYPE: u8 = 127;
const RTP_HEADER_LEN: usize = 12;
/// The audio clock (32 kHz) and the step of one 20 ms frame.
const CLOCK_PER_MS: u64 = 32;
const FRAME_STEP: u32 = 640;

/// Jenkins lookup3 `hashlittle` of `key` with seed 0.
pub fn hash(key: &[u8]) -> u32 {
    let mut a = 0xdead_beef_u32.wrapping_add(key.len() as u32);
    let (mut b, mut c) = (a, a);
    let word = |bytes: &[u8]| {
        let mut padded = [0_u8; 4];
        padded[..bytes.len()].copy_from_slice(bytes);
        u32::from_le_bytes(padded)
    };
    let mut rest = key;
    while rest.len() > 12 {
        a = a.wrapping_add(word(&rest[0..4]));
        b = b.wrapping_add(word(&rest[4..8]));
        c = c.wrapping_add(word(&rest[8..12]));
        (a, b, c) = mix(a, b, c);
        rest = &rest[12..];
    }
    if rest.is_empty() {
        return c;
    }
    let mut tail = [0_u8; 12];
    tail[..rest.len()].copy_from_slice(rest);
    a = a.wrapping_add(word(&tail[0..4]));
    b = b.wrapping_add(word(&tail[4..8]));
    c = c.wrapping_add(word(&tail[8..12]));
    final_mix(a, b, c)
}

const fn mix(mut a: u32, mut b: u32, mut c: u32) -> (u32, u32, u32) {
    a = a.wrapping_sub(c);
    a ^= c.rotate_left(4);
    c = c.wrapping_add(b);
    b = b.wrapping_sub(a);
    b ^= a.rotate_left(6);
    a = a.wrapping_add(c);
    c = c.wrapping_sub(b);
    c ^= b.rotate_left(8);
    b = b.wrapping_add(a);
    a = a.wrapping_sub(c);
    a ^= c.rotate_left(16);
    c = c.wrapping_add(b);
    b = b.wrapping_sub(a);
    b ^= a.rotate_left(19);
    a = a.wrapping_add(c);
    c = c.wrapping_sub(b);
    c ^= b.rotate_left(4);
    b = b.wrapping_add(a);
    (a, b, c)
}

const fn final_mix(mut a: u32, mut b: u32, mut c: u32) -> u32 {
    c ^= b;
    c = c.wrapping_sub(b.rotate_left(14));
    a ^= c;
    a = a.wrapping_sub(c.rotate_left(11));
    b ^= a;
    b = b.wrapping_sub(a.rotate_left(25));
    c ^= b;
    c = c.wrapping_sub(b.rotate_left(16));
    a ^= c;
    a = a.wrapping_sub(c.rotate_left(4));
    b ^= a;
    b = b.wrapping_sub(a.rotate_left(14));
    c ^= b;
    c.wrapping_sub(b.rotate_left(24))
}

/// The SIP address of `name` (a user or a channel) in `realm`, as the hashes and the roster
/// name it: `sip:<name>@<realm>` with a space as `%20` and `@` as `%40` in the name.
pub fn sip_address(name: &str, realm: &str) -> String {
    format!(
        "sip:{}@{realm}",
        name.replace(' ', "%20").replace('@', "%40")
    )
}

/// One member of a room in a roster: its `userHash`, its room's `sessionHash` and its address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// `userHash`.
    pub user_hash: u32,
    /// `sessionHash` of the room it is in.
    pub session_hash: u32,
    /// Its SIP address.
    pub address: String,
}

/// The roster payloads of `members`: as many as their entries need, each a payload header and
/// one roster block of at most [`MAX_BLOCK_LEN`] bytes.
pub fn roster_payloads(members: &[Member]) -> Vec<Vec<u8>> {
    let mut payloads = Vec::new();
    let mut entries: Vec<u8> = Vec::new();
    for member in members {
        let entry = roster_entry(member);
        if BLOCK_HEADER_LEN + BLOCK_WORDS_LEN + entries.len() + entry.len() > MAX_BLOCK_LEN
            && !entries.is_empty()
        {
            payloads.push(roster_payload(&entries));
            entries.clear();
        }
        entries.extend(entry);
    }
    if !entries.is_empty() {
        payloads.push(roster_payload(&entries));
    }
    payloads
}

fn roster_entry(member: &Member) -> Vec<u8> {
    let mut address = member.address.as_bytes().to_vec();
    address.push(0);
    let mut entry = Vec::with_capacity(ENTRY_HEADER_LEN + address.len());
    entry.extend((address.len() as u32).to_be_bytes());
    entry.extend(IN_AUDIO.to_be_bytes());
    entry.extend(member.session_hash.to_be_bytes());
    entry.extend(member.user_hash.to_be_bytes());
    entry.extend(address);
    entry
}

fn roster_payload(entries: &[u8]) -> Vec<u8> {
    let length = BLOCK_HEADER_LEN + BLOCK_WORDS_LEN + entries.len();
    let mut payload = PAYLOAD_HEADER.to_vec();
    payload.push((length >> 2) as u8);
    payload.push((((length & 0b11) as u8) << 6) | ROSTER_BLOCK_TYPE);
    payload.extend([0_u8; 12]);
    payload.extend(ROSTER_FLAGS.to_be_bytes());
    payload.extend(entries);
    payload
}

/// The RTP header of one receiver: its own SSRC, sequence and timestamp, the timestamp following
/// the wall clock at 32 kHz and always one frame or more after the last one.
#[derive(Debug, Clone)]
pub struct OutStream {
    ssrc: u32,
    sequence: u16,
    last_timestamp: Option<u32>,
    started: Instant,
}

impl OutStream {
    /// A stream with SSRC `ssrc`, started now.
    pub fn new(ssrc: u32) -> Self {
        Self {
            ssrc,
            sequence: 0,
            last_timestamp: None,
            started: Instant::now(),
        }
    }

    /// `payload` as this receiver's next RTP packet.
    pub fn packet(&mut self, payload: &[u8]) -> Vec<u8> {
        let clock = (self.started.elapsed().as_millis() as u64 * CLOCK_PER_MS) as u32;
        let aligned = clock - clock % FRAME_STEP;
        let timestamp = match self.last_timestamp {
            Some(last) if aligned <= last => last.wrapping_add(FRAME_STEP),
            _ => aligned,
        };
        self.last_timestamp = Some(timestamp);
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        let mut packet = Vec::with_capacity(RTP_HEADER_LEN + payload.len());
        packet.push(RTP_FIRST_BYTE);
        packet.push(AUDIO_PAYLOAD_TYPE);
        packet.extend(sequence.to_be_bytes());
        packet.extend(timestamp.to_be_bytes());
        packet.extend(self.ssrc.to_be_bytes());
        packet.extend(payload);
        packet
    }
}

/// The payload of the RTP packet `packet` (after its 12-byte header); `None` for a packet with
/// CSRCs, an extension or padding, which the SDK never sends.
pub fn payload(packet: &[u8]) -> Option<&[u8]> {
    (packet.len() >= RTP_HEADER_LEN && packet[0] == RTP_FIRST_BYTE)
        .then(|| &packet[RTP_HEADER_LEN..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const REALM: &str = "game.example.org";

    #[test]
    fn the_hash_is_lookup3_of_the_player_and_room_addresses() {
        assert_eq!(hash(b""), 0xdead_beef);
        assert_eq!(hash(b"Four score and seven years ago"), 0x1777_0551);
        let player = sip_address(".community-1.00000000-0000-0000-0000-000000000001.", REALM);
        assert_eq!(hash(player.as_bytes()), 0x5c81_4ff2, "the token address");
        let account = sip_address(
            ".community-1.00000000-0000-0000-0000-000000000001+B.",
            REALM,
        );
        assert_eq!(
            hash(account.as_bytes()),
            0x9361_e5e1,
            "the userHash of the account jid"
        );
        let room = sip_address("confctl-d-community-1.019012_0", REALM);
        assert_eq!(hash(room.as_bytes()), 0x7a29_922a, "the sessionHash");
    }

    #[test]
    fn a_roster_entry_has_its_length_flags_hashes_and_address() {
        let member = Member {
            user_hash: 0x9361_e5e1,
            session_hash: 0x7a29_922a,
            address: "sip:a@b".to_owned(),
        };
        let payloads = roster_payloads(std::slice::from_ref(&member));
        assert_eq!(payloads.len(), 1);
        let payload = &payloads[0];
        let block_len = BLOCK_HEADER_LEN + BLOCK_WORDS_LEN + ENTRY_HEADER_LEN + 8;
        assert_eq!(&payload[..2], PAYLOAD_HEADER);
        assert_eq!(
            usize::from(payload[2]) * 4 + usize::from(payload[3] >> 6),
            block_len
        );
        assert_eq!(payload[3] & 0x3f, ROSTER_BLOCK_TYPE);
        assert_eq!(&payload[16..20], ROSTER_FLAGS.to_be_bytes());
        let entry = &payload[20..];
        assert_eq!(entry[..4], 8_u32.to_be_bytes(), "the address and its NUL");
        assert_eq!(entry[4..8], IN_AUDIO.to_be_bytes());
        assert_eq!(entry[8..12], 0x7a29_922a_u32.to_be_bytes());
        assert_eq!(entry[12..16], 0x9361_e5e1_u32.to_be_bytes());
        assert_eq!(&entry[16..], b"sip:a@b\0");
        let many: Vec<Member> = (0..60).map(|_| member.clone()).collect();
        let split = roster_payloads(&many);
        assert!(split.len() > 1, "a block holds at most 1023 bytes");
        assert!(
            split
                .iter()
                .all(|payload| payload.len() - 2 <= MAX_BLOCK_LEN)
        );
    }

    #[test]
    fn each_packet_of_a_stream_has_the_next_sequence_and_a_later_timestamp() {
        let mut stream = OutStream::new(7);
        let first = stream.packet(&[1]);
        let second = stream.packet(&[2]);
        assert_eq!(&first[..2], [RTP_FIRST_BYTE, AUDIO_PAYLOAD_TYPE]);
        assert_eq!(&first[8..12], 7_u32.to_be_bytes());
        assert_eq!(u16::from_be_bytes([second[2], second[3]]), 1);
        let stamp =
            |packet: &[u8]| u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
        assert!(stamp(&second) >= stamp(&first) + FRAME_STEP);
        assert_eq!(payload(&second), Some(&[2_u8][..]));
        assert_eq!(payload(&[0x90; 20]), None, "an extension: not the SDK's");
    }
}
