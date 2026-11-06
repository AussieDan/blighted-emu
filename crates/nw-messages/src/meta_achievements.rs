// SPDX-License-Identifier: AGPL-3.0-or-later
//! The meta achievements of the achievements window (progression note, section 8): the
//! client's calls of the `MetaAchievementComponent` and the server's answers and notices. Each
//! answer goes on the call's facet header; a notice on the player's meta achievement component
//! ([`META_ACHIEVEMENT_COMPONENT`]). A meta achievement is the CRC-32 of its lowercase
//! `MetaAchievementId`, a window tab the CRC-32 of its lowercase `UIDisplayCategory`. Each list
//! is a wide prefix-coded count and its entries (retail `9603` for 214 entries).
//!
//! | Call | Answer |
//! |---|---|
//! | 3092 `ServerRequestMetaAchievementsSummary` (header alone) | 1679 `ClientReceiveMetaAchievementsSummaryInformation` |
//! | 3794 `ServerRequestMetaAchievementsProgressForCategory` (a tab) | 3800 `ClientReceiveMetaAchievementsProgress` |
//! | 3227 `ServerRequestClearPendingMetaAchievements` (one byte) | none |
//!
//! The server also tells of a progress (331 `ClientOnChangedProgressStatus`) when a meta
//! achievement completes (Observed, S8 ledger: 8 notices, each with the meta achievement's
//! `Total` or more).

use crate::facet::FacetHeader;
use crate::wire::{BodyCodec, MessageError, Reader, Writer};

/// `MetaAchievementComponentServerFacet_ServerRequestMetaAchievementsSummary`.
pub const REQUEST_SUMMARY_TYPE_INDEX: u64 = 3092;
/// `MetaAchievementComponentServerFacet_ServerRequestMetaAchievementsProgressForCategory`.
pub const REQUEST_CATEGORY_PROGRESS_TYPE_INDEX: u64 = 3794;
/// `MetaAchievementComponentServerFacet_ServerRequestClearPendingMetaAchievements`.
pub const CLEAR_PENDING_TYPE_INDEX: u64 = 3227;
/// `MetaAchievementComponentClientFacet_ClientReceiveMetaAchievementsSummaryInformation`.
pub const SUMMARY_TYPE_INDEX: u64 = 1679;
/// `MetaAchievementComponentClientFacet_ClientReceiveMetaAchievementsProgress`.
pub const CATEGORY_PROGRESS_TYPE_INDEX: u64 = 3800;
/// `MetaAchievementComponentClientFacet_ClientOnChangedProgressStatus`.
pub const PROGRESS_CHANGED_TYPE_INDEX: u64 = 331;
/// The target-id constant of the player's `MetaAchievementComponent` (Observed: the target
/// less the entity of the 3092, 843 and 331 headers of 5 retail characters: menus-linux,
/// newchar-retail, S8, claim_achievement_titles, Hive_achievements).
pub const META_ACHIEVEMENT_COMPONENT: u64 = 0x2e4c_dde6_4547_e9ad;
/// The third field of a 331 notice (meaning Unknown): 0 in all retail notices.
pub const RETAIL_NOTICE_THIRD: u32 = 0;
/// The fourth field of a 331 notice (meaning Unknown): `ffffffff` in all retail notices.
pub const RETAIL_NOTICE_FOURTH: u32 = u32::MAX;
/// The last 8 bytes of a 331 notice (meaning Unknown): zero in the notices of 5 of the 7
/// characters with one (two others repeat one other value in each of their notices).
pub const RETAIL_NOTICE_LAST: u64 = 0;
/// Bytes of one id.
const ID_LEN: usize = 4;
/// Bytes of one (id, progress) entry.
const ENTRY_LEN: usize = 8;

/// One meta achievement and its progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaProgress {
    /// The meta achievement.
    pub id: u32,
    /// Its progress.
    pub progress: u32,
}

/// One meta achievement call of the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaAchievementRequest {
    /// 3092: the summary (the window opens; retail sends it at each spawn too).
    Summary {
        /// The header (the player's `MetaAchievementComponent`).
        header: FacetHeader,
    },
    /// 3794: the progress of one window tab.
    CategoryProgress {
        /// The header.
        header: FacetHeader,
        /// The tab.
        category: u32,
    },
    /// 3227: the player saw the completed meta achievements whose reward waited.
    ClearPending {
        /// The header.
        header: FacetHeader,
        /// One byte of Unknown meaning (0 in all 7 retail calls).
        flag: u8,
    },
}

impl MetaAchievementRequest {
    /// Decodes `body` when `type_index` is a meta achievement call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |read_rest: fn(FacetHeader, &mut Reader<'_>) -> Result<Self, MessageError>| {
            let mut reader = Reader::new(body);
            let request = read_rest(FacetHeader::read(&mut reader)?, &mut reader)?;
            reader.finish()?;
            Ok(request)
        };
        Some(match type_index {
            REQUEST_SUMMARY_TYPE_INDEX => read(|header, _| Ok(Self::Summary { header })),
            REQUEST_CATEGORY_PROGRESS_TYPE_INDEX => read(|header, reader| {
                Ok(Self::CategoryProgress {
                    header,
                    category: reader.u32()?,
                })
            }),
            CLEAR_PENDING_TYPE_INDEX => read(|header, reader| {
                Ok(Self::ClearPending {
                    header,
                    flag: reader.u8()?,
                })
            }),
            _ => return None,
        })
    }

    /// The header of the call.
    pub const fn header(&self) -> FacetHeader {
        match *self {
            Self::Summary { header }
            | Self::CategoryProgress { header, .. }
            | Self::ClearPending { header, .. } => header,
        }
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Summary { .. } => REQUEST_SUMMARY_TYPE_INDEX,
            Self::CategoryProgress { .. } => REQUEST_CATEGORY_PROGRESS_TYPE_INDEX,
            Self::ClearPending { .. } => CLEAR_PENDING_TYPE_INDEX,
        }
    }

    /// The body bytes, as the client sends them.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        self.header().write(&mut writer);
        match *self {
            Self::Summary { .. } => {}
            Self::CategoryProgress { category, .. } => {
                writer.u32(category);
            }
            Self::ClearPending { flag, .. } => {
                writer.u8(flag);
            }
        }
        writer.finish()
    }
}

/// 1679: the summary of the achievements window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaAchievementSummaryMsg {
    /// The header of the call.
    pub header: FacetHeader,
    /// The two latest completions, the older first (Observed: retail always sent two; Inferred
    /// from their order in the completed list and the 331 of the newchar login).
    pub latest: Vec<u32>,
    /// The meta achievements nearest completion, nearest first (Observed: two of 82 % to 97 %
    /// in four sessions, none in two others).
    pub nearest: Vec<MetaProgress>,
    /// Every completed meta achievement.
    pub completed: Vec<u32>,
}

impl BodyCodec for MetaAchievementSummaryMsg {
    const TYPE_INDEX: u64 = SUMMARY_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        write_ids(&mut writer, &self.latest);
        write_entries(&mut writer, &self.nearest);
        write_ids(&mut writer, &self.completed);
        Ok(writer.finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let latest = read_ids(&mut reader)?;
        let nearest = read_entries(&mut reader)?;
        let completed = read_ids(&mut reader)?;
        reader.finish()?;
        Ok(Self {
            header,
            latest,
            nearest,
            completed,
        })
    }
}

/// 3800: the progress of every meta achievement of one window tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaAchievementProgressMsg {
    /// The header of the call.
    pub header: FacetHeader,
    /// Each meta achievement of the tab and its progress, by ascending id (Observed).
    pub progress: Vec<MetaProgress>,
    /// The completed ones of them.
    pub completed: Vec<u32>,
}

impl BodyCodec for MetaAchievementProgressMsg {
    const TYPE_INDEX: u64 = CATEGORY_PROGRESS_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        write_entries(&mut writer, &self.progress);
        write_ids(&mut writer, &self.completed);
        Ok(writer.finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let progress = read_entries(&mut reader)?;
        let completed = read_ids(&mut reader)?;
        reader.finish()?;
        Ok(Self {
            header,
            progress,
            completed,
        })
    }
}

/// 331: a meta achievement's progress reached a notice (its completion, or 75 % for one whose
/// `NotificationVerbosity` says so).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaProgressChangedMsg {
    /// The header (the player's meta achievement component).
    pub header: FacetHeader,
    /// The meta achievement and its progress.
    pub entry: MetaProgress,
    /// The third field ([`RETAIL_NOTICE_THIRD`]).
    pub third: u32,
    /// The fourth field ([`RETAIL_NOTICE_FOURTH`]).
    pub fourth: u32,
    /// The last field ([`RETAIL_NOTICE_LAST`]).
    pub last: u64,
}

impl MetaProgressChangedMsg {
    /// The notice of `entry` for the player of entity number `entity_number`, with the
    /// retail values of the fields of Unknown meaning.
    pub fn of(entity_number: u64, entry: MetaProgress) -> Self {
        Self {
            header: FacetHeader::of(META_ACHIEVEMENT_COMPONENT, entity_number),
            entry,
            third: RETAIL_NOTICE_THIRD,
            fourth: RETAIL_NOTICE_FOURTH,
            last: RETAIL_NOTICE_LAST,
        }
    }
}

impl BodyCodec for MetaProgressChangedMsg {
    const TYPE_INDEX: u64 = PROGRESS_CHANGED_TYPE_INDEX;

    fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        self.header.write(&mut writer);
        Ok(writer
            .u32(self.entry.id)
            .u32(self.entry.progress)
            .u32(self.third)
            .u32(self.fourth)
            .u64(self.last)
            .finish())
    }

    fn decode(body: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(body);
        let header = FacetHeader::read(&mut reader)?;
        let entry = MetaProgress {
            id: reader.u32()?,
            progress: reader.u32()?,
        };
        let third = reader.u32()?;
        let fourth = reader.u32()?;
        let last = reader.u64()?;
        reader.finish()?;
        Ok(Self {
            header,
            entry,
            third,
            fourth,
            last,
        })
    }
}

fn write_ids(writer: &mut Writer, ids: &[u32]) {
    // A `usize` count fits 64 bits.
    writer.wide_prefixed(ids.len() as u64);
    for id in ids {
        writer.u32(*id);
    }
}

fn write_entries(writer: &mut Writer, entries: &[MetaProgress]) {
    // A `usize` count fits 64 bits.
    writer.wide_prefixed(entries.len() as u64);
    for entry in entries {
        writer.u32(entry.id).u32(entry.progress);
    }
}

/// A count no larger than the entries of `size` bytes the body still holds.
fn count_of(reader: &mut Reader<'_>, size: usize) -> Result<u64, MessageError> {
    let count = reader.wide_prefixed()?;
    let limit = u64::try_from(reader.remaining() / size).unwrap_or(u64::MAX);
    if count > limit {
        return Err(MessageError::CountTooLarge { count, limit });
    }
    Ok(count)
}

fn read_ids(reader: &mut Reader<'_>) -> Result<Vec<u32>, MessageError> {
    (0..count_of(reader, ID_LEN)?)
        .map(|_| reader.u32())
        .collect()
}

fn read_entries(reader: &mut Reader<'_>) -> Result<Vec<MetaProgress>, MessageError> {
    (0..count_of(reader, ENTRY_LEN)?)
        .map(|_| {
            Ok(MetaProgress {
                id: reader.u32()?,
                progress: reader.u32()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: FacetHeader = FacetHeader {
        target: 0x0102_0304_0506_0708,
        entity: 0x1112_1314_1516_1718,
    };
    /// Retail 1679 of claim_achievement_titles, after the header: latest
    /// `FirstDiscoverableRecipeFound` and `Quest_Windsward_Mercy`, nearest `CharacterProgression3`
    /// at 25 and `POIVisitedCount2` at 41, and the first 2 of the 27 completed.
    const RETAIL_SUMMARY: &str =
        "0293052da65c14093d02fedc041f00000019a0ab72bd00000029025c14093dd8d60a49";
    /// Retail 3800 of `Places_Discovered` cut to its first two entries, and the 331 of the S8
    /// ledger with its last field zero (as five other characters sent it).
    const RETAIL_PROGRESS: &str = "0217d4ba960000000039a2230700000014013ecfe71e";
    const RETAIL_NOTICE: &str = "f4d196110000001400000000ffffffff0000000000000000";
    /// 214 completed (`9603`, menus-linux 2026-09-12).
    const MANY: usize = 214;
    const MANY_PREFIX: [u8; 2] = [0x96, 0x03];

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn with_header(rest: &[u8]) -> Vec<u8> {
        let mut writer = Writer::new();
        HEADER.write(&mut writer);
        writer.bytes(rest);
        writer.finish()
    }

    #[test]
    fn the_retail_summary_and_progress_read_and_write_back() {
        let body = with_header(&hex(RETAIL_SUMMARY));
        let summary = MetaAchievementSummaryMsg::decode(&body).expect("decode");
        assert_eq!(summary.latest, [0x9305_2da6, 0x5c14_093d]);
        assert_eq!(
            summary.nearest,
            [
                MetaProgress {
                    id: 0xfedc_041f,
                    progress: 25,
                },
                MetaProgress {
                    id: 0xa0ab_72bd,
                    progress: 41,
                },
            ]
        );
        assert_eq!(summary.completed.len(), 2);
        assert_eq!(summary.encode().expect("encode"), body);
        let body = with_header(&hex(RETAIL_PROGRESS));
        let progress = MetaAchievementProgressMsg::decode(&body).expect("decode");
        assert_eq!(progress.progress.len(), 2);
        assert_eq!(progress.completed, [0x3ecf_e71e]);
        assert_eq!(progress.encode().expect("encode"), body);
        let many = MetaAchievementSummaryMsg {
            header: HEADER,
            latest: Vec::new(),
            nearest: Vec::new(),
            completed: vec![0; MANY],
        };
        let bytes = many.encode().expect("encode");
        assert_eq!(&bytes[18..20], &MANY_PREFIX);
    }

    #[test]
    fn a_notice_is_the_retail_bytes() {
        let notice = MetaProgressChangedMsg {
            header: HEADER,
            entry: MetaProgress {
                id: 0xf4d1_9611,
                progress: 20,
            },
            third: RETAIL_NOTICE_THIRD,
            fourth: RETAIL_NOTICE_FOURTH,
            last: RETAIL_NOTICE_LAST,
        };
        let bytes = notice.encode().expect("encode");
        assert_eq!(bytes, with_header(&hex(RETAIL_NOTICE)));
        assert_eq!(
            MetaProgressChangedMsg::decode(&bytes).expect("decode"),
            notice
        );
        let of = MetaProgressChangedMsg::of(7, notice.entry);
        assert_eq!(
            of.header.target.wrapping_sub(of.header.entity),
            META_ACHIEVEMENT_COMPONENT
        );
    }

    #[test]
    fn the_calls_read_and_long_lists_are_refused() {
        let summary = MetaAchievementRequest::decode(REQUEST_SUMMARY_TYPE_INDEX, &with_header(&[]))
            .expect("a call")
            .expect("reads");
        assert_eq!(summary, MetaAchievementRequest::Summary { header: HEADER });
        assert_eq!(summary.type_index(), REQUEST_SUMMARY_TYPE_INDEX);
        assert_eq!(summary.encode(), with_header(&[]));
        let tab = with_header(&[0xa5, 0xaa, 0x3c, 0x61]);
        let category = MetaAchievementRequest::decode(REQUEST_CATEGORY_PROGRESS_TYPE_INDEX, &tab)
            .expect("a call")
            .expect("reads");
        assert_eq!(
            category,
            MetaAchievementRequest::CategoryProgress {
                header: HEADER,
                category: 0xa5aa_3c61,
            }
        );
        assert_eq!(category.type_index(), REQUEST_CATEGORY_PROGRESS_TYPE_INDEX);
        assert_eq!(category.encode(), tab);
        let clear = MetaAchievementRequest::decode(CLEAR_PENDING_TYPE_INDEX, &with_header(&[0]))
            .expect("a call")
            .expect("reads");
        assert_eq!(clear.header(), HEADER);
        assert_eq!(clear.type_index(), CLEAR_PENDING_TYPE_INDEX);
        assert_eq!(clear.encode(), with_header(&[0]));
        assert!(MetaAchievementRequest::decode(1, &with_header(&[])).is_none());
        assert!(matches!(
            MetaAchievementRequest::decode(REQUEST_SUMMARY_TYPE_INDEX, &with_header(&[0])),
            Some(Err(_))
        ));
        assert!(matches!(
            MetaAchievementSummaryMsg::decode(&with_header(&[0x05])),
            Err(MessageError::CountTooLarge { .. })
        ));
    }
}
