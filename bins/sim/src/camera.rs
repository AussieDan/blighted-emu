// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client's camera and keys of each input record, for the server's own movement of the
//! player ([`nw_sim_core::TrackedBody`], movement study 2026-09-20).
//!
//! The client sends its newest record first and writes each record against the record before it
//! in the message, the newer one (`FUN_142a28f30` passes the previous record to the record writer
//! `FUN_142a26320`, which sets a mask bit only for a field that differs): a record without its
//! frame time (bit 5), view angles (bit 6) or camera point (bit 7) has the newer record's. The
//! newest record of every retail input after the spawn carries all three (41,669 records of the
//! newchar capture; the 828 without were sent before the spawn).

use nw_messages::{InputRecord, LookDir};
use nw_sim_core::{CameraView, CharacterInput, Intent, Position, TrackInput};

/// The frame time of a record that never said one, s: the client's 30 Hz input (Observed: 0.0333
/// s in every unthrottled retail record, 0.05 s while the server throttles the input to 20 Hz).
const DEFAULT_FRAME_S: f32 = 1.0 / 30.0;

/// The camera, view and frame time of one record, after the fields it left out.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RecordView {
    /// The frame time, s.
    pub frame_s: Option<f32>,
    /// The camera yaw and pitch.
    pub view: Option<LookDir>,
    /// The camera point.
    pub camera: Option<Position>,
}

/// The view of each record of `records` (newest first, as the client sends them), each record
/// taking the fields it leaves out from the newer one.
pub fn record_views(records: &[InputRecord]) -> Vec<RecordView> {
    let mut newer = RecordView::default();
    records
        .iter()
        .map(|record| {
            newer = RecordView {
                frame_s: record.bit_5.or(newer.frame_s),
                view: record
                    .bit_6
                    .map(|[yaw, pitch]| LookDir { yaw, pitch })
                    .or(newer.view),
                camera: record
                    .bit_7
                    .map(|camera| Position {
                        x: camera.x,
                        y: camera.y,
                        z: camera.z(),
                    })
                    .or(newer.camera),
            };
            newer
        })
        .collect()
}

/// The movement input of `record` with its view `seen`; `None` without view angles (a client
/// that sends only a point: the test client, the load-test bots).
pub fn track_input(record: &InputRecord, seen: RecordView) -> Option<TrackInput> {
    let look = seen.view?;
    let keys = record.move_keys();
    let intent = Intent {
        jump: keys.jump,
        walk_toggle: record.walk_toggle_pressed(),
        block: record.blocking(),
        dodge: record.dodging(),
        crouch_toggle: record.crouch_toggle_pressed(),
        prone_toggle: record.prone_toggle_pressed(),
        walk: record.walk_held(),
        ..Intent::of_keys(keys.forward, keys.back, keys.left, keys.right)
    };
    Some(TrackInput {
        character: CharacterInput {
            intent,
            yaw: look.yaw,
            dt_s: seen.frame_s.unwrap_or(DEFAULT_FRAME_S),
        },
        view: seen
            .camera
            .map(|camera| CameraView::new(camera, look.yaw, look.pitch)),
        boom_key: record.boom_key_pressed(),
        action_started: record.slayer_input().is_some(),
    })
}

#[cfg(test)]
mod tests {
    use nw_messages::{InputPosition, InputSlots};

    use super::*;

    const CLOSE: f32 = 1e-3;

    fn camera(x: f32) -> InputPosition {
        InputPosition::from_metres(x, 850.0, 92.0).expect("finite")
    }

    #[test]
    fn an_older_record_takes_what_it_leaves_out_from_the_newer_one() {
        // Newest first: the newest record carries everything, the older one only a new camera.
        let newest = InputRecord {
            u16_after_mask: 11,
            bit_5: Some(0.05),
            bit_6: Some([2.6, 0.2]),
            bit_7: Some(camera(478.0)),
            ..InputRecord::default()
        };
        let older = InputRecord {
            u16_after_mask: 10,
            bit_7: Some(camera(477.5)),
            ..InputRecord::default()
        };
        let views = record_views(&[newest, older]);
        assert_eq!(views.len(), 2);
        assert_eq!(views[1].frame_s, Some(0.05));
        assert_eq!(
            views[1].view,
            Some(LookDir {
                yaw: 2.6,
                pitch: 0.2
            })
        );
        assert!((views[1].camera.expect("camera").x - 477.5).abs() < CLOSE);
        assert!((views[0].camera.expect("camera").x - 478.0).abs() < CLOSE);
    }

    #[test]
    fn a_record_gives_the_keys_the_view_and_the_camera() {
        // Retail running forward (clean running capture): value 6 bit 32 and value 0 bits 14
        // and 43, with the standing values 7 and 8.
        let record = InputRecord {
            slots: InputSlots {
                count: 19,
                values: vec![
                    0x0800_0000_4000,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0x1_0000_0000,
                    0x0402_0000_0800_0000,
                    0x0200_0000_4000_0000,
                    0,
                ],
                bytes: [128; 4],
            },
            ..InputRecord::default()
        };
        let seen = RecordView {
            frame_s: None,
            view: Some(LookDir {
                yaw: 2.5,
                pitch: 0.1,
            }),
            camera: Some(Position {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            }),
        };
        let input = track_input(&record, seen).expect("a view");
        assert_eq!(
            input.character.intent,
            Intent::of_keys(true, false, false, false)
        );
        assert!((input.character.dt_s - DEFAULT_FRAME_S).abs() < f32::EPSILON);
        assert!((input.character.yaw - 2.5).abs() < f32::EPSILON);
        assert!(input.view.is_some());
        assert!(!input.boom_key && !input.action_started);
        let no_camera = RecordView {
            camera: None,
            ..seen
        };
        assert_eq!(track_input(&record, no_camera).expect("a view").view, None);
        let point_only = RecordView { view: None, ..seen };
        assert_eq!(track_input(&record, point_only), None);
    }

    #[test]
    fn the_block_and_dodge_keys_go_to_the_character() {
        // Slot value 1: bit 62 while block is held, bit 3 while the dodge key is (input note).
        const BLOCK_AND_DODGE: u64 = 1 << 62 | 1 << 3;
        let record = InputRecord {
            slots: InputSlots {
                count: 19,
                values: vec![0, BLOCK_AND_DODGE],
                bytes: [128; 4],
            },
            ..InputRecord::default()
        };
        let seen = RecordView {
            frame_s: None,
            view: Some(LookDir {
                yaw: 0.0,
                pitch: 0.0,
            }),
            camera: None,
        };
        let intent = track_input(&record, seen).expect("a view").character.intent;
        assert!(intent.block && intent.dodge, "{intent:?}");
    }

    #[test]
    fn the_posture_toggles_and_the_walk_key_go_to_the_character() {
        // Slot value 4: bit 21 a crouch press, bit 57 the walk key; value 5 bit 11 a prone press
        // (posture study 2026-09-20).
        const CROUCH_AND_WALK: u64 = 1 << 21 | 1 << 57;
        const PRONE: u64 = 1 << 11;
        let record = InputRecord {
            slots: InputSlots {
                count: 19,
                values: vec![0, 0, 0, 0, CROUCH_AND_WALK, PRONE],
                bytes: [128; 4],
            },
            ..InputRecord::default()
        };
        let seen = RecordView {
            frame_s: None,
            view: Some(LookDir {
                yaw: 0.0,
                pitch: 0.0,
            }),
            camera: None,
        };
        let intent = track_input(&record, seen).expect("a view").character.intent;
        assert!(
            intent.crouch_toggle && intent.prone_toggle && intent.walk,
            "{intent:?}"
        );
    }
}
