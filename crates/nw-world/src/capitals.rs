// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reader for `*.capitals.json` placement files (M1b spec section 3.3). Unknown JSON keys are
//! ignored; a missing required key is an error.

use serde::Deserialize;

use crate::height::TerrainCookie;
use crate::records::Capital;

/// The tag of a capital's entity id: its low 3 bits (GDE id note, section 3).
const CAPITAL_TAG: u64 = 3;
/// The bit a capital's id clears before its tag.
const CAPITAL_CLEARED_BIT: u64 = 4;
/// Hex digits of one half of a capital's GUID.
const GUID_HALF_DIGITS: usize = 16;
/// Radix of a GUID's digits.
const HEX_RADIX: u32 = 16;
/// The GUID separator.
const GUID_SEPARATOR: char = '-';

/// The capitals of one file and the terrain cookies among them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapitalsRead {
    /// Every capital of the file.
    pub capitals: Vec<Capital>,
    /// The `cookie` of each capital that has one, at the capital's position.
    pub cookies: Vec<TerrainCookie>,
}

#[derive(Deserialize)]
struct CapitalsFile {
    #[serde(rename = "Capitals")]
    capitals: Vec<CapitalJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CapitalJson {
    id: String,
    world_position: Vector3Json,
    rotation: QuaternionJson,
    scale: Option<f32>,
    slice_name: String,
    variant_name: Option<String>,
    slice_asset_id: String,
    cookie: Option<CookieJson>,
}

/// A capital's `cookie`. A missing key reads as the client's default (`FUN_1469b55c0`): 0.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CookieJson {
    #[serde(default)]
    inner_radius: f32,
    #[serde(default)]
    outer_radius: f32,
    #[serde(default)]
    max_height_blend: f32,
}

#[derive(Deserialize)]
struct Vector3Json {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Deserialize)]
struct QuaternionJson {
    x: f32,
    y: f32,
    z: f32,
    w: f32,
}

/// Reads the capitals of one `*.capitals.json` file.
pub fn parse_capitals(bytes: &[u8]) -> Result<Vec<Capital>, serde_json::Error> {
    parse_capitals_and_cookies(bytes).map(|read| read.capitals)
}

/// Reads the capitals of one `*.capitals.json` file and their terrain cookies.
pub fn parse_capitals_and_cookies(bytes: &[u8]) -> Result<CapitalsRead, serde_json::Error> {
    let file: CapitalsFile = serde_json::from_slice(bytes)?;
    let mut read = CapitalsRead::default();
    for capital in file.capitals {
        let position = [
            capital.world_position.x,
            capital.world_position.y,
            capital.world_position.z,
        ];
        if let Some(cookie) = capital.cookie {
            read.cookies.push(TerrainCookie {
                center: position,
                inner_radius: cookie.inner_radius,
                outer_radius: cookie.outer_radius,
                max_height_blend: cookie.max_height_blend,
            });
        }
        read.capitals.push(Capital {
            id: capital.id,
            position,
            rotation: [
                capital.rotation.x,
                capital.rotation.y,
                capital.rotation.z,
                capital.rotation.w,
            ],
            scale: capital.scale,
            slice_name: capital.slice_name,
            variant_name: capital.variant_name,
            slice_asset_id: capital.slice_asset_id,
        });
    }
    Ok(read)
}

/// The entity id of the capital of GUID text `guid`: `(hi ^ lo) & !4 | 3`, the halves read
/// big-endian (GDE id note, section 3; all 7,584 tag-3 ids of the captures).
pub fn capital_id(guid: &str) -> Option<u64> {
    let digits: String = guid.chars().filter(|c| *c != GUID_SEPARATOR).collect();
    if digits.len() != 2 * GUID_HALF_DIGITS {
        return None;
    }
    let (high, low) = digits.split_at(GUID_HALF_DIGITS);
    let high = u64::from_str_radix(high, HEX_RADIX).ok()?;
    let low = u64::from_str_radix(low, HEX_RADIX).ok()?;
    Some((high ^ low) & !CAPITAL_CLEARED_BIT | CAPITAL_TAG)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capital_id_is_the_xor_of_its_guid_halves_tagged_3() {
        // The start beach watchtower (newchar 2026-09-18).
        assert_eq!(
            capital_id("f6593b66-abc3-7460-7a4d-5afaeb8b2620"),
            Some(0x8c14_619c_4048_5243)
        );
        assert_eq!(capital_id("not-a-guid"), None);
    }

    const FULL: &str = r#"{"Capitals": [{
        "id": "79091f06-f24f-466f-2641-f8743605f9b7",
        "worldPosition": {"x": 14174.326171875, "y": 8069.5, "z": 56.98},
        "rotation": {"x": 0.0, "y": 0.0, "z": -0.25, "w": 0.75},
        "scale": 0.5,
        "footprint": {"type": "Circle", "id": "3d7f29bc", "radius": 1.0},
        "sliceName": "Master_LootContainer",
        "variantName": "Ancient_T3_M",
        "sliceAssetId": "{B00A72FE-BA8F-5CD6-A5FC-014F7E771769}:2"
    }]}"#;
    const PLOT: &str = r#"{"Capitals": [{
        "id": "a", "worldPosition": {"x": 1, "y": 2, "z": 3},
        "rotation": {"x": 0, "y": 0, "z": 0, "w": 1},
        "footprint": {"type": "Rectangle", "id": "b", "width": 4, "height": 5},
        "sliceName": "<PLOT>", "sliceAssetId": "{00000000-0000-0000-0000-000000000000}:0"
    }]}"#;
    const COOKIE: &str = r#"{"Capitals": [{
        "id": "c", "worldPosition": {"x": 7049.9, "y": 3094.4, "z": 45.0},
        "rotation": {"x": 0, "y": 0, "z": 0, "w": 1},
        "cookie": {"outerRadius": 27.0, "maxHeightBlend": 1.0, "edgeNoise": 0.1},
        "sliceName": "<PLOT>", "sliceAssetId": "{00000000-0000-0000-0000-000000000000}:0"
    }]}"#;
    const MISSING_ID: &str = r#"{"Capitals": [{
        "worldPosition": {"x": 1, "y": 2, "z": 3}, "rotation": {"x": 0, "y": 0, "z": 0, "w": 1},
        "sliceName": "<PLOT>", "sliceAssetId": "x"
    }]}"#;

    #[test]
    fn reads_every_field_and_ignores_unknown_keys() {
        let capitals = parse_capitals(FULL.as_bytes()).unwrap();
        assert_eq!(
            capitals,
            [Capital {
                id: "79091f06-f24f-466f-2641-f8743605f9b7".to_owned(),
                position: [14174.326, 8069.5, 56.98],
                rotation: [0.0, 0.0, -0.25, 0.75],
                scale: Some(0.5),
                slice_name: "Master_LootContainer".to_owned(),
                variant_name: Some("Ancient_T3_M".to_owned()),
                slice_asset_id: "{B00A72FE-BA8F-5CD6-A5FC-014F7E771769}:2".to_owned(),
            }]
        );
    }

    #[test]
    fn plot_entry_has_no_scale_or_variant() {
        let capital = &parse_capitals(PLOT.as_bytes()).unwrap()[0];
        assert_eq!(capital.slice_name, "<PLOT>");
        assert_eq!(capital.position, [1.0, 2.0, 3.0]);
        assert_eq!(capital.scale, None);
        assert_eq!(capital.variant_name, None);
    }

    #[test]
    fn a_cookie_is_read_at_its_capitals_place_with_the_clients_defaults() {
        let read = parse_capitals_and_cookies(COOKIE.as_bytes()).unwrap();
        assert_eq!(read.capitals.len(), 1);
        assert_eq!(
            read.cookies,
            [TerrainCookie {
                center: [7049.9, 3094.4, 45.0],
                inner_radius: 0.0,
                outer_radius: 27.0,
                max_height_blend: 1.0,
            }]
        );
        assert!(
            parse_capitals_and_cookies(FULL.as_bytes())
                .unwrap()
                .cookies
                .is_empty()
        );
    }

    #[test]
    fn missing_required_key_is_an_error() {
        assert!(parse_capitals(MISSING_ID.as_bytes()).is_err());
    }
}
