// SPDX-License-Identifier: AGPL-3.0-or-later
//! The salvage of bag items (craft report, run 15): `InitiateSalvage` (1733) names one bag slot
//! and its item's instance, `SalvageItemsInInventory` (6188) a list of bag slots. Each named
//! item leaves the bag, and its salvage gives what [`nw_content::salvage`] reads from its row:
//! the roll of its `RepairRecipe` table with its tier, rarity and gear score, or a fifth of its
//! "Salvage Values" recipe, and its `SalvageGameEventID`'s coins. The same call opens a loot
//! container item (retail: `ConcealedVaultLVL30` gave four alchemy reagents and coins,
//! `CRX_RewardContainerT3_Chestwear_30` a chest piece).
//!
//! Retail answers with no message of its own: the bag's changes (the item gone, the items
//! gained) and the wallet's (ledgers `Playthrough_Part_03`, `MSQ6-lv28`, `20261008-145727-200`).
//! The client sends no count with 6188; each slot's whole stack goes (Inferred: the 13 retail
//! slots held one item each).

use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_content::perks::rarity;
use nw_content::salvage::SalvageSource;
use nw_messages::wire::RAW_ID_LEN;

use super::loot_bag::LimitsSent;
use super::{Player, SimWorld};
use crate::objectives::Event;

/// One bag item a salvage call names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SalvageTarget {
    /// Its bag slot.
    pub(super) bag_slot: u32,
    /// The instance id the call names (1733); `None` for a call that names slots only (6188).
    pub(super) instance: Option<[u8; RAW_ID_LEN]>,
    /// How many of the stack; `None` for the whole stack.
    pub(super) quantity: Option<u32>,
}

/// Why the salvage of one bag item is refused; the item stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SalvageRefusal {
    /// The slot is empty, or the player has no bag.
    Empty,
    /// The slot holds another instance than the call names.
    Instance,
    /// Its owner locked it (the Azoth catalog note: the server refuses locked items).
    Locked,
    /// Its row is not `IsSalvageable`.
    NotSalvageable,
    /// The stack holds fewer than the call asks for.
    Quantity,
}

/// An empty perk slot of an item.
const NO_PERK: u32 = 0;

/// Whether a rarity mask takes an item of `rarity` (bit `rarity` set).
fn rarity_taken(rarities: u8, rarity: u32) -> bool {
    1_u32
        .checked_shl(rarity)
        .is_some_and(|bit| u32::from(rarities) & bit != 0)
}

impl SimWorld {
    /// Player `number` (`entity`) salvages the bag items of `targets` at `now_ms`, one after
    /// the other; a refused one stays and the others go on.
    pub(super) fn salvage(
        &mut self,
        entity: Entity,
        number: u64,
        targets: &[SalvageTarget],
        now_ms: u64,
    ) {
        for target in targets {
            if let Err(refusal) = self.salvage_one(entity, number, *target, now_ms) {
                tracing::debug!(player = number, ?target, ?refusal, "salvage refused");
            }
        }
    }

    /// Player `number` (`entity`) salvages every bag item of UI item class `class` whose rarity
    /// bit is set in `rarities` (6024; craft2 report: retail 0x47 took 141 weapons and tools,
    /// 0x48 116 armor and jewelry pieces, 0x1f every rarity). A locked item stays (14 of 16
    /// retail), and so does one that is not salvageable ([`SimWorld::salvage`] refuses it).
    pub(super) fn salvage_by_class(
        &mut self,
        entity: Entity,
        number: u64,
        class: u64,
        rarities: u8,
        now_ms: u64,
    ) {
        let content = Arc::clone(&self.content);
        let targets: Vec<SalvageTarget> = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map(|gear| {
                gear.bag_stacks()
                    .into_iter()
                    .filter(|(_, item_id, _, locked)| {
                        !locked && content.is_of_ui_class(item_id, class)
                    })
                    .filter(|(bag_slot, _, _, _)| {
                        gear.bag_item(*bag_slot).is_some_and(|(_, item)| {
                            let perks = item.perks.iter().filter(|perk| **perk != NO_PERK).count();
                            rarity_taken(rarities, rarity(perks))
                        })
                    })
                    .map(|(bag_slot, _, instance, _)| SalvageTarget {
                        bag_slot,
                        instance: Some(instance),
                        quantity: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        tracing::debug!(
            player = number,
            class,
            rarities,
            items = targets.len(),
            "salvage by class"
        );
        self.salvage(entity, number, &targets, now_ms);
    }

    /// Salvages the bag item of `target`: it leaves the bag, its items go into the bag (less
    /// what the loot limits hold back), its game event's coins into the wallet, and its achievement
    /// (a schematic's recipe unlock) is set.
    fn salvage_one(
        &mut self,
        entity: Entity,
        number: u64,
        target: SalvageTarget,
        now_ms: u64,
    ) -> Result<(), SalvageRefusal> {
        let content = Arc::clone(&self.content);
        let (item_id, gear_score, quantity) = {
            let mut player = self
                .world
                .get_mut::<Player>(entity)
                .ok_or(SalvageRefusal::Empty)?;
            let gear = player.gear.as_mut().ok_or(SalvageRefusal::Empty)?;
            let (item_id, item) = gear
                .bag_item(target.bag_slot)
                .ok_or(SalvageRefusal::Empty)?;
            if target
                .instance
                .is_some_and(|instance| instance != item.instance)
            {
                return Err(SalvageRefusal::Instance);
            }
            if item.locked {
                return Err(SalvageRefusal::Locked);
            }
            let quantity = target.quantity.unwrap_or(item.quantity);
            let (item_id, gear_score) = (item_id.to_owned(), item.gear_score);
            if content.salvage.of(&item_id).is_none() {
                return Err(SalvageRefusal::NotSalvageable);
            }
            if !gear.take_from_slot(target.bag_slot, quantity) {
                return Err(SalvageRefusal::Quantity);
            }
            (item_id, gear_score, quantity)
        };
        let Some(rule) = content.salvage.of(&item_id) else {
            return Err(SalvageRefusal::NotSalvageable);
        };
        let mut items = Vec::new();
        for _ in 0..quantity {
            match &rule.source {
                Some(SalvageSource::Table(table)) => {
                    let reached = self.reached_loot_limits(number);
                    let context = self.loot_context(number, None, &reached);
                    let dice = &mut self.dice;
                    let rolled = content.loot.roll_salvage(
                        table,
                        context,
                        rule.levels(gear_score),
                        &mut |below| dice.below(below),
                    );
                    items.extend(self.limit_loot(number, rolled));
                }
                Some(SalvageSource::Recipe(_)) => items.extend(rule.returned()),
                None => {}
            }
        }
        self.send_loot_limits(number, LimitsSent::Changed);
        tracing::debug!(player = number, item = %item_id, quantity, ?items, "salvaged");
        self.give_loot(number, items, now_ms);
        if let Some(event) = &rule.event {
            self.grant_event(number, event, quantity);
        }
        if let Some(achievement) = &rule.achievement {
            self.earn_achievement(number, achievement, now_ms);
        }
        self.quest_event(
            number,
            Event::Salvaged {
                item: &item_id,
                quantity,
            },
            now_ms,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use std::sync::Arc;

    use nw_content::achievements::AchievementIndexes;
    use nw_content::rewards::{EventGrant, EventGrants};
    use nw_content::salvage::{Salvage, SalvageRow};

    use super::*;
    use crate::player_content::PlayerContent;
    use crate::world::gear;
    use crate::world::level_flow::AchievementRules;
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, connection, world_with_spawns};

    /// The player's number.
    const PLAYER: u64 = 1;

    /// The kit items of the test content: a starter piece and a resource.
    const CHEST: &str = "SoldierArchetypeChest_Upgrade";
    const INGOT: &str = "IngotT2";
    /// Its "Salvage Values" recipe: 5 ingots.
    const RECIPE: &str = "SalvageSmall_MetalT2";
    const COIN_EVENT: &str = "SalvageCoinGrantT2";
    const COINS: u64 = 25;

    fn content() -> PlayerContent {
        PlayerContent {
            kits: gear::tests::kits_with(&[
                (CHEST, "EquippableChest+Armor+Heavy"),
                (INGOT, "Resource+Ingot"),
            ]),
            salvage: Salvage::build(
                [SalvageRow {
                    item: CHEST.to_owned(),
                    salvageable: true,
                    resources: Some(1),
                    repair_recipe: Some(RECIPE.to_owned()),
                    event: Some(COIN_EVENT.to_owned()),
                    achievement: None,
                    tier: Some(1),
                    rarity: None,
                }],
                [(RECIPE.to_owned(), vec![(INGOT.to_owned(), 5)])],
            ),
            event_grants: EventGrants::build([(
                COIN_EVENT.to_owned(),
                EventGrant {
                    progression: None,
                    coins: (COINS, COINS),
                    territory_standing: 0.0,
                },
            )]),
            ..PlayerContent::default()
        }
    }

    /// The bag slot of `item` in `gear`.
    fn slot_of(gear: &gear::Gear, item: &str) -> Option<u32> {
        let slots = u32::try_from(nw_messages::STARTER_BAG_SLOTS).expect("slots");
        (0..slots).find(|slot| gear.bag_item(*slot).is_some_and(|(id, _)| id == item))
    }

    /// A world with `content` and one player with the test backstory's kit, outside the
    /// tutorial.
    fn admitted(content: PlayerContent) -> (SimWorld, Entity) {
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(content);
        let mut admit = admit_at(PLAYER, 0.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let entity = world.by_connection[&connection(PLAYER)];
        (world, entity)
    }

    #[test]
    fn a_salvage_takes_the_item_and_gives_its_resources_and_coins() {
        let (mut world, entity) = admitted(content());
        let number = PLAYER;
        let (chest, ingot) = {
            let kits = &world.content.kits;
            (
                kits.item(CHEST).cloned().expect("chest"),
                kits.item(INGOT).cloned().expect("ingot"),
            )
        };
        let (slot, instance) = {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            let gear = player.gear.as_mut().expect("gear");
            assert!(gear.add(&chest, 1));
            assert!(gear.add(&ingot, 1));
            let slot = slot_of(gear, CHEST).expect("slot");
            (slot, gear.bag_item(slot).expect("item").1.instance)
        };
        let coins_before = world.world.get::<Player>(entity).expect("player").coins;
        let wrong = SalvageTarget {
            bag_slot: slot,
            instance: Some([0; RAW_ID_LEN]),
            quantity: Some(1),
        };
        world.salvage(entity, number, &[wrong], NOW);
        let count = |world: &SimWorld, item: &str| {
            world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref())
                .map_or(0, |gear| gear.bag_count(item))
        };
        assert_eq!(count(&world, CHEST), 1, "another instance: refused");
        let target = SalvageTarget {
            instance: Some(instance),
            ..wrong
        };
        world.salvage(entity, number, &[target], NOW);
        assert_eq!(count(&world, CHEST), 0, "salvaged");
        assert_eq!(count(&world, INGOT), 2, "a fifth of the recipe's 5 ingots");
        let ingot_slot = {
            let player = world.world.get::<Player>(entity).expect("player");
            assert_eq!(player.coins, coins_before + COINS);
            assert!(player.coins_changed);
            let gear = player.gear.as_ref().expect("gear");
            slot_of(gear, INGOT).expect("ingot slot")
        };
        let by_slot = SalvageTarget {
            bag_slot: ingot_slot,
            instance: None,
            quantity: None,
        };
        world.salvage(entity, number, &[by_slot], NOW);
        assert_eq!(count(&world, INGOT), 2, "an ingot is not salvageable");
    }

    #[test]
    fn a_schematic_salvage_sets_the_achievement_its_recipe_asks_for() {
        /// A weapon schematic of build 25810808 and the achievement its recipe asks for.
        const SCHEMATIC: &str = "Schematic_ShatterslashT4";
        const UNLOCK: &str = "ShatterslashT4";
        const UNLOCK_BIT: u32 = 3;
        let mut content = content();
        content.kits = gear::tests::kits_with(&[(SCHEMATIC, "Resource+WeaponSchematic")]);
        content.salvage = Salvage::build(
            [SalvageRow {
                item: SCHEMATIC.to_owned(),
                salvageable: true,
                achievement: Some(UNLOCK.to_owned()),
                ..SalvageRow::default()
            }],
            [],
        );
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(content);
        world.achievement_rules = AchievementRules::of(
            &AchievementIndexes::build([(UNLOCK.to_owned(), UNLOCK_BIT)]),
            &[],
        );
        let mut admit = admit_at(PLAYER, 0.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let entity = world.by_connection[&connection(PLAYER)];
        let schematic = world.content.kits.item(SCHEMATIC).cloned().expect("item");
        let slot = {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            let gear = player.gear.as_mut().expect("gear");
            assert!(gear.add(&schematic, 1));
            slot_of(gear, SCHEMATIC).expect("slot")
        };
        let rules = world.achievement_rules.clone();
        let player = world.world.get::<Player>(entity).expect("player");
        assert!(!player.flow.has_achievement(&rules, UNLOCK));
        let target = SalvageTarget {
            bag_slot: slot,
            instance: None,
            quantity: None,
        };
        world.salvage(entity, PLAYER, &[target], NOW);
        let player = world.world.get::<Player>(entity).expect("player");
        assert!(
            player.flow.has_achievement(&rules, UNLOCK),
            "the recipe is learned"
        );
    }

    #[test]
    fn a_rarity_mask_takes_the_rarities_of_its_bits() {
        // Retail 0x1f: common to legendary.
        assert!((0..=4).all(|rarity| rarity_taken(0x1f, rarity)));
        assert!(!rarity_taken(0x1f, 5));
        assert!(rarity_taken(0b100, 2) && !rarity_taken(0b100, 1));
        assert!(!rarity_taken(0xff, 40));
    }
}
