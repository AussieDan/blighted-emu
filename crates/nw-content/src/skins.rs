// SPDX-License-Identifier: AGPL-3.0-or-later
//! Item skins and dyes (run-15 items2 study): the skin each item gives to the transmog
//! collection, which items a skin fits, the skins every character has for free, and the dye
//! colors and dye items.
//!
//! **Skin ids (Observed).** A skin is the CRC-32 of a lowercase appearance id: all 423 skins of
//! a retail `SkinCollection` match. An armor item gives the `AppearanceName` of the
//! `ArmorAppearanceDefinitions` row its `ArmorAppearanceM` names (2676 of 2677 rows whose
//! appearance is collected; the name is shared by both genders); a weapon or tool gives its
//! `WeaponAppearanceOverride`, else its own `ItemID` (1448 of 1450).
//!
//! **What a skin fits (best guess).** A skin goes on an item of the same kind: armor of the same
//! paperdoll slot (`EquippableHead`, ...), a weapon or tool of the same type (the class part
//! after its `Equippable` part, as in `Weapon+EquippableTwoHand+GreatSword+Melee`).
//!
//! **Free skins (Observed: `ItemSkinData`).** 2612 of 2621 rows are entitlements (store,
//! Twitch, season and leaderboard rewards) and are never granted here. Of the 9 others, the 5
//! without an `Outfit` (the hide-armor skins) are every character's; the 4 of the
//! `RabbitsRevenge2024` outfit come from that event (best guess: an outfit is an event reward).
//!
//! **Published look (Observed: 427 armor slots of retail `PublishedData`, the character select
//! look).** Each armor slot holds the CRC-32 of the lowercase `ArmorAppearanceDefinitions`
//! `ItemID` the character shows there, of its gender (`m_DynastyWarriorHeavy_Chest`): store
//! skins appear in them, so a worn skin counts over its item ([`SkinCatalog::published_look`]).

use std::collections::HashMap;

use nw_formats::objectstream::lowercase_crc32;

use crate::Tables;
use crate::perks::parts;
use crate::starter::EquipSlot;

/// The armor slots of a published look, in [`PublishedLook`] order.
pub const PUBLISHED_SLOTS: [EquipSlot; 5] = [
    EquipSlot::Head,
    EquipSlot::Chest,
    EquipSlot::Hands,
    EquipSlot::Legs,
    EquipSlot::Feet,
];
/// The appearance a character shows in each slot of [`PUBLISHED_SLOTS`]: the CRC-32 of the
/// lowercase gendered appearance id, 0 for a slot it wears nothing in.
pub type PublishedLook = [u32; PUBLISHED_SLOTS.len()];
/// A published look slot that shows nothing.
pub const NO_APPEARANCE: u32 = 0;

/// The start of the class part that names where an item is worn.
const EQUIPPABLE_PREFIX: &str = "Equippable";
/// The class parts of the armor slots: a skin of one of these goes on armor of the same slot.
const ARMOR_SLOTS: [&str; 5] = [
    "EquippableHead",
    "EquippableChest",
    "EquippableHands",
    "EquippableLegs",
    "EquippableFeet",
];
/// The class parts of items whose appearance is a weapon appearance.
const WEAPON_SLOTS: [&str; 4] = [
    "EquippableMainHand",
    "EquippableTwoHand",
    "EquippableOffHand",
    "EquippableTool",
];

/// The kind of item a skin goes on, from an item's class parts: the armor slot part, else the
/// part after the `Equippable` part (the weapon or tool type), else that part.
pub fn skin_kind(classes: &[String]) -> Option<String> {
    let at = classes
        .iter()
        .position(|part| part.starts_with(EQUIPPABLE_PREFIX))?;
    let slot = &classes[at];
    if ARMOR_SLOTS
        .iter()
        .any(|armor| armor.eq_ignore_ascii_case(slot))
    {
        return Some(slot.to_lowercase());
    }
    Some(classes.get(at + 1).unwrap_or(slot).to_lowercase())
}

/// One `MasterItemDefinitions` row as the skins need it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkinItemRow {
    /// `ItemID`.
    pub item_id: String,
    /// The parts of `ItemClass`.
    pub classes: Vec<String>,
    /// `ArmorAppearanceM`.
    pub armor_appearance: Option<String>,
    /// `ArmorAppearanceF`.
    pub armor_appearance_female: Option<String>,
    /// `WeaponAppearanceOverride`.
    pub weapon_appearance: Option<String>,
}

/// What a [`SkinCatalog`] is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkinSource {
    /// The item rows.
    pub items: Vec<SkinItemRow>,
    /// `ArmorAppearanceDefinitions`: (`ItemID`, `AppearanceName`).
    pub armor_appearances: Vec<(String, Option<String>)>,
    /// The `ToItemID` of the `ItemSkinData` rows every character has.
    pub free_items: Vec<String>,
}

impl SkinSource {
    /// The skin rows of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        let filled = |cell: &Option<String>| cell.clone().filter(|text| !text.trim().is_empty());
        Self {
            items: tables
                .master_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| SkinItemRow {
                    item_id: row.item_id.clone(),
                    classes: parts(row.item_class.as_deref()),
                    armor_appearance: filled(&row.armor_appearance_m),
                    armor_appearance_female: filled(&row.armor_appearance_f),
                    weapon_appearance: filled(&row.weapon_appearance_override),
                })
                .collect(),
            armor_appearances: tables
                .armor_appearance_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| (row.item_id.clone(), filled(&row.appearance_name)))
                .collect(),
            free_items: tables
                .item_skin_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter(|row| row.is_entitlement == 0 && filled(&row.outfit).is_none())
                .map(|row| row.to_item_id.clone())
                .collect(),
        }
    }
}

/// The skins of the items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkinCatalog {
    /// The skin of each lowercase item id.
    by_item: HashMap<String, u32>,
    /// The kind each skin goes on ([`skin_kind`]).
    kinds: HashMap<u32, String>,
    /// The skins every character has.
    free: Vec<u32>,
    /// The published slot and look of each lowercase armor item id.
    armor_looks: HashMap<String, (usize, GenderedLook)>,
    /// The look of each armor skin: that of the first item that gives it.
    skin_looks: HashMap<u32, GenderedLook>,
}

/// The appearance CRCs of one armor look, male and female.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GenderedLook {
    male: u32,
    female: u32,
}

impl GenderedLook {
    const fn of(&self, female: bool) -> u32 {
        if female { self.female } else { self.male }
    }
}

impl SkinCatalog {
    /// The skins of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(SkinSource::from_tables(tables))
    }

    /// The skins of `source` (module notes).
    pub fn build(source: SkinSource) -> Self {
        let names: HashMap<String, Option<String>> = source
            .armor_appearances
            .into_iter()
            .map(|(item_id, name)| (item_id.to_lowercase(), name))
            .collect();
        let mut catalog = Self::default();
        for row in &source.items {
            let weapon = row.classes.iter().any(|part| {
                WEAPON_SLOTS
                    .iter()
                    .any(|slot| slot.eq_ignore_ascii_case(part))
            });
            let appearance = if weapon {
                Some(
                    row.weapon_appearance
                        .clone()
                        .unwrap_or_else(|| row.item_id.clone()),
                )
            } else {
                row.armor_appearance.as_ref().map(|appearance| {
                    names
                        .get(&appearance.to_lowercase())
                        .cloned()
                        .flatten()
                        .unwrap_or_else(|| appearance.clone())
                })
            };
            let (Some(appearance), Some(kind)) = (appearance, skin_kind(&row.classes)) else {
                continue;
            };
            let skin = lowercase_crc32(&appearance);
            catalog.by_item.insert(row.item_id.to_lowercase(), skin);
            catalog.kinds.entry(skin).or_insert(kind);
            let slot = row.classes.iter().find_map(|part| {
                PUBLISHED_SLOTS
                    .iter()
                    .position(|slot| EquipSlot::of_item_class(part) == Some(*slot))
            });
            if let (Some(slot), Some(male)) = (slot, &row.armor_appearance) {
                let female = row.armor_appearance_female.as_ref().unwrap_or(male);
                let look = GenderedLook {
                    male: lowercase_crc32(male),
                    female: lowercase_crc32(female),
                };
                catalog
                    .armor_looks
                    .insert(row.item_id.to_lowercase(), (slot, look));
                catalog.skin_looks.entry(skin).or_insert(look);
            }
        }
        catalog.free = source
            .free_items
            .iter()
            .filter_map(|item_id| catalog.skin_of(item_id))
            .collect();
        catalog
    }

    /// The skin item `item_id` (any case) gives; `None` for an item without an appearance.
    pub fn skin_of(&self, item_id: &str) -> Option<u32> {
        self.by_item.get(&item_id.to_lowercase()).copied()
    }

    /// The skins every character has.
    pub fn free(&self) -> &[u32] {
        &self.free
    }

    /// The published look of a character of gender `female` who wears `worn` (item id, applied
    /// skin or 0): each armor item's appearance, or its skin's when one is applied ([module
    /// notes](self)). Items that are not armor are left out.
    pub fn published_look<'a>(
        &self,
        worn: impl IntoIterator<Item = (&'a str, u32)>,
        female: bool,
    ) -> PublishedLook {
        let mut look = [NO_APPEARANCE; PUBLISHED_SLOTS.len()];
        for (item_id, skin) in worn {
            let Some((slot, item_look)) = self.armor_looks.get(&item_id.to_lowercase()) else {
                continue;
            };
            let shown = self.skin_looks.get(&skin).unwrap_or(item_look);
            look[*slot] = shown.of(female);
        }
        look
    }

    /// Whether `skin` goes on an item of `classes`.
    pub fn fits(&self, skin: u32, classes: &[String]) -> bool {
        self.kinds
            .get(&skin)
            .is_some_and(|kind| skin_kind(classes).as_deref() == Some(kind.as_str()))
    }
}

/// The dye colors and the dye items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DyeCatalog {
    /// Whether each `DyeColorData` index is an entitlement (a store color).
    colors: HashMap<u8, bool>,
    /// The color of each lowercase dye item id (`DyeItemDefinitions`).
    items: HashMap<String, u8>,
}

impl DyeCatalog {
    /// The dyes of `tables`; a color index past one byte is left out.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .dye_color_data
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| Some((u8::try_from(row.index).ok()?, row.is_entitlement != 0))),
            tables
                .dye_item_definitions
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    Some((row.dye_item_id.clone(), u8::try_from(row.color_index).ok()?))
                }),
        )
    }

    /// The dyes of `colors` (index, is entitlement) and `items` (dye item id, color index).
    pub fn build(
        colors: impl IntoIterator<Item = (u8, bool)>,
        items: impl IntoIterator<Item = (String, u8)>,
    ) -> Self {
        Self {
            colors: colors.into_iter().collect(),
            items: items
                .into_iter()
                .map(|(item_id, color)| (item_id.to_lowercase(), color))
                .collect(),
        }
    }

    /// Whether color `index` may be used: a known color that is no entitlement.
    pub fn is_free_color(&self, index: u8) -> bool {
        self.colors.get(&index) == Some(&false)
    }

    /// The color of dye item `item_id` (any case); `None` for an item that is no dye.
    pub fn color_of(&self, item_id: &str) -> Option<u8> {
        self.items.get(&item_id.to_lowercase()).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(item_id: &str, classes: &str, armor: Option<&str>, weapon: Option<&str>) -> SkinItemRow {
        SkinItemRow {
            item_id: item_id.to_owned(),
            classes: parts(Some(classes)),
            armor_appearance: armor.map(str::to_owned),
            armor_appearance_female: armor.map(|male| format!("f_{male}")),
            weapon_appearance: weapon.map(str::to_owned),
        }
    }

    fn catalog() -> SkinCatalog {
        SkinCatalog::build(SkinSource {
            items: vec![
                row(
                    "HeavyChest_MonarchsBluffT3",
                    "EquippableChest+Armor+Heavy",
                    Some("Conquistador_Chest_T2"),
                    None,
                ),
                row(
                    "LightChestT1",
                    "EquippableChest+Armor+Light",
                    Some("Cloth_Shirt_T1"),
                    None,
                ),
                row(
                    "2hBowMonarchsBluffT3",
                    "Weapon+EquippableTwoHand+Bow+Ranged",
                    None,
                    Some("2hBowT3"),
                ),
                row("2hBowT4", "Weapon+EquippableTwoHand+Bow+Ranged", None, None),
                row(
                    "2hGreatSwordT2",
                    "Weapon+EquippableTwoHand+GreatSword+Melee",
                    None,
                    None,
                ),
                row(
                    "Skin_Head_HideHelmet",
                    "EquippableHead+Skin",
                    Some("HideHelmet"),
                    None,
                ),
                row("PotionHealthT1", "Consumable+Potion", None, None),
            ],
            armor_appearances: vec![(
                "Conquistador_Chest_T2".to_owned(),
                Some("Conquistador_Chest".to_owned()),
            )],
            free_items: vec!["Skin_Head_HideHelmet".to_owned(), "NoSuchItem".to_owned()],
        })
    }

    #[test]
    fn the_published_look_shows_each_slots_item_or_its_skin_by_gender() {
        let catalog = catalog();
        let worn = [
            ("HeavyChest_MonarchsBluffT3", NO_APPEARANCE),
            ("2hBowT4", NO_APPEARANCE),
        ];
        let chest = PUBLISHED_SLOTS
            .iter()
            .position(|slot| *slot == EquipSlot::Chest)
            .expect("chest");
        let male = catalog.published_look(worn, false);
        assert_eq!(male[chest], lowercase_crc32("Conquistador_Chest_T2"));
        assert_eq!(
            male.iter().filter(|crc| **crc != NO_APPEARANCE).count(),
            1,
            "a weapon has no armor slot"
        );
        assert_eq!(
            catalog.published_look(worn, true)[chest],
            lowercase_crc32("f_Conquistador_Chest_T2")
        );
        let cloth = catalog.skin_of("LightChestT1").expect("skin");
        assert_eq!(
            catalog.published_look([("HeavyChest_MonarchsBluffT3", cloth)], false)[chest],
            lowercase_crc32("Cloth_Shirt_T1"),
            "the skin shows over the item"
        );
        assert_eq!(
            catalog.published_look([], false),
            [NO_APPEARANCE; PUBLISHED_SLOTS.len()]
        );
    }

    #[test]
    fn items_give_their_appearance_as_a_skin() {
        let catalog = catalog();
        assert_eq!(
            catalog.skin_of("heavychest_monarchsbluffT3"),
            Some(lowercase_crc32("Conquistador_Chest"))
        );
        assert_eq!(
            catalog.skin_of("LightChestT1"),
            Some(lowercase_crc32("Cloth_Shirt_T1"))
        );
        assert_eq!(
            catalog.skin_of("2hBowMonarchsBluffT3"),
            Some(lowercase_crc32("2hBowT3"))
        );
        assert_eq!(catalog.skin_of("2hBowT4"), Some(lowercase_crc32("2hBowT4")));
        assert_eq!(catalog.skin_of("PotionHealthT1"), None);
        assert_eq!(catalog.free(), [lowercase_crc32("HideHelmet")]);
    }

    #[test]
    fn a_skin_fits_the_same_slot_or_weapon_type() {
        let catalog = catalog();
        let chest = lowercase_crc32("Conquistador_Chest");
        let bow = lowercase_crc32("2hBowT3");
        assert!(catalog.fits(chest, &parts(Some("EquippableChest+Armor+Light"))));
        assert!(!catalog.fits(chest, &parts(Some("EquippableLegs+Armor+Heavy"))));
        assert!(catalog.fits(bow, &parts(Some("Weapon+EquippableTwoHand+Bow+Ranged"))));
        assert!(!catalog.fits(
            bow,
            &parts(Some("Weapon+EquippableTwoHand+GreatSword+Melee"))
        ));
        assert!(!catalog.fits(0, &parts(Some("EquippableChest"))));
    }

    #[test]
    fn dyes_know_their_colors_and_store_colors() {
        let dyes = DyeCatalog::build(
            [(4, false), (148, true)],
            [("DyeB4".to_owned(), 4), ("DyeA148".to_owned(), 148)],
        );
        assert!(dyes.is_free_color(4));
        assert!(!dyes.is_free_color(148));
        assert!(!dyes.is_free_color(5));
        assert_eq!(dyes.color_of("dyeb4"), Some(4));
        assert_eq!(dyes.color_of("PotionHealthT1"), None);
    }
}
