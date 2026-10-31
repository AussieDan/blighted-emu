// SPDX-License-Identifier: AGPL-3.0-or-later
//! `player-attacks`: builds the player attack table (`player-attacks-<build>.json`, read by
//! `nw_content::player_attacks`) from the game paks and the content bundle (combat audit,
//! 2026-09-18).
//!
//! The stances are the `MannequinTag` cells of the player weapons: the weapon rows whose
//! `AnimDbPath` is a player attach database (`PlayerMaleAnims_*`). For each stance, the charge
//! fragment (`Attack_Start_Charge`, else `Attack_Start_Charge_Upperbody`, as the player action
//! lists order them) of the player animation databases holds the transitions of the attack key:
//! the first one taken when the key is let go is the light attack, a later one with another
//! action the heavy attack, the first one taken while the key is held (no condition, or the
//! heavy magic check) with another action the charged attack. Each action's fragment (the action
//! lists) holds its hit: `CAGE-Damage` clips (in the weapon's attach database, else the player
//! one), a `CAGE-RangedAttack` clip, a `CAGE-CastSpell` clip, or a spell the action casts while
//! a fragment marker (a `CAGE-Named` clip) is on. A clip with a `Condition` counts when the named
//! condition holds for a character with the stance's tags and nothing else: no perk, status
//! effect, aim or lock (`Condition::holds`).
//!
//! Each melee attack gets its hit shapes' paths ([`crate::melee_shapes`]), its fragment's length
//! and its combo links: its `CAGE-TransitionInstant` and `CAGE-TransitionBuffered` clips whose new
//! action is a charge action (an action whose fragment holds the release transitions) start that
//! action's step; the chain's steps are numbered in the order the links find them, the stance's
//! own charge action first. An action of the action lists that sets a Mannequin tag while a status
//! effect is on (`Greatsword_OffenseStance`: `GenericTagA` on `Greatsword_Stance_Offense`) and
//! whose activation holds for the stance with that effect gives the stance a variant built with
//! the tag, when the variant differs.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use nw_content::Tables;
use nw_content::player_attacks::{
    AttackHit, AttackMotion, BasicAttack, ChargeStep, ComboLink, Flight, Homing, ItemUse, Stance,
    StanceVariant, stance_tags,
};
use nw_content::sheathe::SheatheAction;
use nw_formats::azcs::{decode_azcs, is_azcs};
use nw_formats::caf::Caf;
use nw_formats::character::CharacterDefinition;
use nw_formats::mannequin::{
    Action, ActionList, AnimClip, AnimDatabase, DrivenMotion, Fragment, ProcClip,
};
use nw_formats::objectstream::{Element, lowercase_crc32, parse_object_stream};
use nw_formats::skeleton::Skeleton;

use crate::error::ToolError;
use crate::melee_shapes::{
    Bodies, CLIP_FOLDERS, CLIP_SUFFIX, ClipContext, DEFINITION_SUFFIX, EQUIP_TYPES, MS_PER_SECOND,
    PLAYER_DEFINITION, PLAYER_SKELETON, WeaponBody, equip_attachments, melee_shape, ms, normalized,
    root_path,
};
use crate::source::GameSource;

/// The root motion multiplier where no row of an attack's motion sets one.
const DEFAULT_MOTION_MULTIPLIER: f32 = 1.0;
/// The player animation databases.
const PLAYER_ADB_PREFIX: &str = "animations/mannequin/adb/player/";
const ADB_SUFFIX: &str = ".adb";
/// The player action lists.
const PLAYER_ACTIONS_PREFIX: &str = "sharedassets/springboardentitites/playerdata/cage/";
const ACTION_LIST_SUFFIX: &str = ".actionlist";
/// The ranged attack database.
pub const RANGED_ATTACK_DATABASE: &str = "sharedassets/genericassets/rangedattackdatabase.radb";
/// A prefab path's slice in the paks.
const SLICE_PREFIX: &str = "slices/";
const SLICE_SUFFIX: &str = ".dynamicslice";
/// The separator of the meshes of a weapon row's `MeshOverride` (Observed: the dagger row
/// `wep_me1h_daggert4_lookbase.cdf|wep_axe2h_greataxea_t2.cdf`).
const MESH_SEPARATOR: char = '|';
/// The attach database of a player weapon (`AnimDbPath`).
const PLAYER_ATTACH_PREFIX: &str = "animations/mannequin/adb/playermaleanims_";

/// The charge actions, in the order the player action lists hold them.
const CHARGE_ACTIONS: [&str; 2] = ["Attack_Start_Charge", "Attack_Start_Charge_Upperbody"];
/// The dodge action and the item use of quick slot 1.
const DODGE_ACTION: &str = "Dodge";
const USE_ACTION: &str = "Use_QuickSlot_1";
/// The sheathe action of main-hand option 1 (`playeractions_sheathe.actionlist`).
const SHEATHE_ACTION: &str = "Sheathe_MainHand_1_UB";
/// The aim actions whose fragment changes to the aimed shot (`playeractions_firearms` and
/// `playeractions_archery` action lists).
const AIM_ACTIONS: [&str; 2] = ["Firearm_AimShoot_SubState", "Bow_AimChargeShoot_SubState"];
/// The Mannequin tag of a character while it aims (`Firearm_Shoot_Primary_UpperBody` entry
/// `2H_Ranged+Aim`).
const AIM_TAG: &str = "Aim";

/// Clip types and parameters.
const TRANSITION_INSTANT: &str = "CAGE-TransitionInstant";
const TRANSITION_BUFFERED: &str = "CAGE-TransitionBuffered";
const DAMAGE_CLIP: &str = "CAGE-Damage";
/// The clip of a ranged attack (a player's or a mob's).
pub const RANGED_CLIP: &str = "CAGE-RangedAttack";
const CAST_CLIP: &str = "CAGE-CastSpell";
const NAMED_CLIP: &str = "CAGE-Named";
const NAMED_CLIP_NAME: &str = "Name";
/// The marker whose start lets the next attack action start (the action lists unset
/// `AttackAction` on it: "Disable AttackAction on CanTransition").
const CAN_TRANSITION_MARKER: &str = "CanTransition";
const HOMING_CLIP: &str = "Homing";
const INVULNERABILITY_CLIP: &str = "Invulnerability";
const USE_CLIP: &str = "UsePaperdollItem";
const CONDITION: &str = "Condition";
const NEW_ACTION: &str = "NewAction";
const NEW_FRAGMENT: &str = "NewFragment";
const DAMAGE_ROW: &str = "DamageTableRow";
const RANGED_ATTACK_NAME: &str = "RangedAttackName";
const SPELL_NAME: &str = "SpellName";
const TRUE_TEXT: &str = "true";

/// The conditions of the charge fragment that wait for the key's release.
const RELEASE_CONDITIONS: [&str; 5] = [
    "Attack_Heavy_NotHeld",
    "Attack_Heavy_NotHeld_Ammo",
    "Attack_Heavy_NotHeld_Magic",
    "Attack_Primary_NotHeld",
    "Attack_Primary_Released",
];
/// The conditions of the charge fragment that fire while the key is held.
const HELD_CONDITIONS: [&str; 3] = ["", "Test_AlwaysTrue", "CanAttack_Heavy_Magic"];

/// The `Homing` parameters.
const HOMING_RADIUS: &str = "Radius";
const HOMING_MAX_ANGLE: &str = "MaxAngle";
const HOMING_HEIGHT: &str = "Height";
const HOMING_RADIUS_WEIGHT: &str = "RadiusWeight";
const HOMING_ANGLE_WEIGHT: &str = "AngleWeight";
const HOMING_AI_RADIUS: &str = "UseRadiusforAITarget";

/// The ranged attack database fields.
const PROFILES_FIELD: &str = "m_rangedattackprofiles";
const ATTACKS_FIELD: &str = "m_rangedattackdata";
const KEY_FIELD: &str = "value1";
const VALUE_FIELD: &str = "value2";
const MAX_RANGE_FIELD: &str = "m_maxrange";
const RADIUS_FIELD: &str = "m_projectileradius";
const HIT_SCAN_FIELD: &str = "m_ishitscan";
/// The projectile slice fields.
const BASE_SPEED_FIELD: &str = "m_basespeed";
const EXTRA_SPEED_FIELD: &str = "m_extraspeed";
const TIME_FOR_MAX_SPEED_FIELD: &str = "m_timeformaxspeed";
const ACCEL_FIELD: &str = "m_accel";
const LIFETIME_FIELD: &str = "m_flyinglifetime";
/// The height axis of `m_accel`.
const UP_AXIS: usize = 2;
/// The ammo row a weapon of an ammo type fires when the player's ammo is not known: the
/// `Default<AmmoType>` row (`DefaultArrow`, `DefaultShot`; Observed ids).
const DEFAULT_AMMO_PREFIX: &str = "Default";

/// What the build did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerAttacksReport {
    /// Stances written.
    pub stances: usize,
    /// Attacks that could not be built, and why.
    pub skipped: Vec<(String, String)>,
}

fn number(clip: &ProcClip, key: &str) -> Option<f32> {
    clip.param(key)?.trim().parse().ok()
}

/// The player animation databases and action lists, the weapons' attach databases, the ranged
/// attack database and the projectile slices.
pub struct AnimationSources {
    /// Every player animation database.
    pub player: Vec<AnimDatabase>,
    /// The attach databases, by lowercase pak path.
    pub attach: HashMap<String, AnimDatabase>,
    /// Every player action list.
    pub actions: Vec<ActionList>,
    /// The sheathe action of the action lists.
    pub sheathe: Option<SheatheAction>,
    /// The ranged attack database: (profile, attack) to (range, radius, hit scan).
    pub ranged: HashMap<(String, String), (f32, f32, bool)>,
    /// The projectile flights, by lowercase prefab path.
    pub flights: HashMap<String, Flight>,
    /// The skeletons, attachments and clips of the hit shapes.
    pub bodies: Bodies,
}

impl AnimationSources {
    /// The best entry of fragment `name` for `tags` over the player databases.
    fn player_fragment(&self, name: &str, tags: &[&str]) -> Option<&Fragment> {
        self.player
            .iter()
            .filter_map(|adb| adb.best(name, tags))
            .fold(None, |best: Option<&Fragment>, fragment| match best {
                Some(best) if best.tags.len() >= fragment.tags.len() => Some(best),
                _ => Some(fragment),
            })
    }

    /// The action `name` of the action lists.
    fn action(&self, name: &str) -> Option<&Action> {
        self.actions.iter().find_map(|list| list.action(name))
    }

    /// The fragment of action `action` (case of the fragment name as the action list writes it).
    fn fragment_of(&self, action: &str) -> Option<&str> {
        self.action(action).map(|action| action.fragment.as_str())
    }

    /// Whether `clip` plays for a character with `tags` and no other state: it has no
    /// `Condition`, or its named condition holds ([`nw_formats::mannequin::Condition::holds`]).
    fn plays(&self, clip: &ProcClip, tags: &[&str]) -> bool {
        match clip.param(CONDITION).filter(|name| !name.is_empty()) {
            None => true,
            Some(name) => self
                .actions
                .iter()
                .find_map(|list| list.condition(name))
                .is_some_and(|condition| condition.holds(tags, &[])),
        }
    }

    /// The entry of the fragment named `fragment` (any case) for `tags`.
    fn player_fragment_any_case(&self, fragment: &str, tags: &[&str]) -> Option<&Fragment> {
        let name = self
            .player
            .iter()
            .flat_map(|adb| &adb.fragments)
            .find(|entry| entry.name.eq_ignore_ascii_case(fragment))?
            .name
            .clone();
        self.player_fragment(&name, tags)
    }
}

/// The ranged attack database in `root`: (profile, attack) to (range, radius, hit scan).
pub fn ranged_attacks(root: &Element) -> HashMap<(String, String), (f32, f32, bool)> {
    let crc = lowercase_crc32;
    let string = |element: &Element, field: &str| {
        element
            .child(crc(field))
            .and_then(|value| value.as_string().ok())
            .map(str::to_owned)
    };
    let mut found = HashMap::new();
    let Some(profiles) = root.child(crc(PROFILES_FIELD)) else {
        return found;
    };
    for profile in &profiles.children {
        let Some(name) = string(profile, KEY_FIELD) else {
            continue;
        };
        let Some(attacks) = profile
            .child(crc(VALUE_FIELD))
            .and_then(|value| value.child(crc(ATTACKS_FIELD)))
        else {
            continue;
        };
        for attack in &attacks.children {
            let Some(attack_name) = string(attack, KEY_FIELD) else {
                continue;
            };
            let Some(data) = attack.child(crc(VALUE_FIELD)) else {
                continue;
            };
            let float = |field: &str| data.child(crc(field)).and_then(|v| v.as_f32().ok());
            let hit_scan = data
                .child(crc(HIT_SCAN_FIELD))
                .and_then(|value| value.as_bool().ok());
            if let (Some(range), Some(radius), Some(hit_scan)) =
                (float(MAX_RANGE_FIELD), float(RADIUS_FIELD), hit_scan)
            {
                found.insert((name.clone(), attack_name), (range, radius, hit_scan));
            }
        }
    }
    found
}

fn find_field(element: &Element, crc: u32) -> Option<&Element> {
    if element.name_crc == Some(crc) {
        return Some(element);
    }
    element
        .children
        .iter()
        .find_map(|child| find_field(child, crc))
}

/// How the projectile of the slice `root` flies; `None` when it has no projectile component.
pub fn slice_flight(root: &Element) -> Option<Flight> {
    let float = |field: &str| find_field(root, lowercase_crc32(field))?.as_f32().ok();
    let accel = find_field(root, lowercase_crc32(ACCEL_FIELD))?
        .as_vec3()
        .ok()?;
    Some(Flight {
        base_speed_mps: float(BASE_SPEED_FIELD)?,
        extra_speed_mps: float(EXTRA_SPEED_FIELD)?,
        time_for_max_speed_s: float(TIME_FOR_MAX_SPEED_FIELD)?,
        gravity_mps2: 0.0 - accel[UP_AXIS],
        lifetime_s: float(LIFETIME_FIELD)?,
    })
}

/// The slice path of prefab `prefab` (`Items/Weapons/basic_arrow`).
fn slice_path(prefab: &str) -> String {
    let prefab = prefab.to_lowercase();
    let prefab = prefab.strip_suffix(SLICE_SUFFIX).unwrap_or(&prefab);
    format!("{SLICE_PREFIX}{prefab}{SLICE_SUFFIX}")
}

/// What the player weapon rows of one `MannequinTag` cell say of their stance: the attach
/// database, ranged profile and ammo prefab of the last row, the equip types and character
/// definitions of all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WeaponStance {
    attach: Option<String>,
    profile: Option<String>,
    ammo_prefab: Option<String>,
    equip_types: BTreeSet<String>,
    definitions: BTreeSet<String>,
}

/// The data of the bundle that the build reads.
pub struct BundleRows {
    /// The player weapons' stances: `MannequinTag` cell to its attach database, ranged profile
    /// and ammo prefab.
    weapons: BTreeMap<String, WeaponStance>,
    /// `SpellData`: spell id to (damage row, ranged profile, ranged attack, prefab).
    spells: HashMap<String, (String, String, String, String)>,
    /// The `MannequinTag`s of the consumables.
    use_tags: BTreeSet<String>,
}

impl BundleRows {
    /// The rows of `tables` the build reads.
    pub fn of(tables: &Tables) -> Self {
        let ammo: HashMap<String, String> = tables
            .ammo_item_definitions
            .iter()
            .flat_map(|table| &table.rows)
            .map(|row| (row.ammo_id.clone(), row.ammo_prefab_path.clone()))
            .collect();
        let mut weapons: BTreeMap<String, WeaponStance> = BTreeMap::new();
        for row in tables
            .weapon_item_definitions
            .iter()
            .flat_map(|table| &table.rows)
        {
            let Some(attach) = row
                .anim_db_path
                .as_deref()
                .map(str::to_lowercase)
                .filter(|attach| attach.starts_with(PLAYER_ATTACH_PREFIX))
            else {
                continue;
            };
            let weapon = weapons
                .entry(row.mannequin_tag.clone().unwrap_or_default())
                .or_default();
            weapon.attach = Some(attach);
            weapon.profile = row.ranged_attack_profile.clone();
            weapon.ammo_prefab = row
                .ammo_type
                .as_ref()
                .and_then(|kind| ammo.get(&format!("{DEFAULT_AMMO_PREFIX}{kind}")).cloned());
            weapon
                .equip_types
                .extend(row.equip_type.as_deref().map(str::to_lowercase));
            weapon.definitions.extend(
                row.mesh_override
                    .iter()
                    .flat_map(|paths| paths.split(MESH_SEPARATOR))
                    .map(normalized)
                    .filter(|path| path.ends_with(DEFINITION_SUFFIX)),
            );
        }
        let spells = tables
            .spell_data
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| {
                Some((
                    row.spell_id.clone(),
                    (
                        row.damage_table_row.clone()?,
                        row.ranged_attack_profile.clone()?,
                        row.ranged_attack_name.clone()?,
                        row.spell_prefab_path.clone()?,
                    ),
                ))
            })
            .collect();
        let use_tags = tables
            .consumable_item_definitions
            .iter()
            .flat_map(|table| &table.rows)
            .filter_map(|row| row.mannequin_tag.clone())
            .filter(|tag| !tag.is_empty())
            .collect();
        Self {
            weapons,
            spells,
            use_tags,
        }
    }

    /// The prefabs whose flights the build needs: the spells' and the default ammo's.
    pub fn prefabs(&self) -> BTreeSet<String> {
        self.spells
            .values()
            .map(|(_, _, _, prefab)| prefab.clone())
            .chain(
                self.weapons
                    .values()
                    .filter_map(|weapon| weapon.ammo_prefab.clone()),
            )
            .collect()
    }

    /// The attach databases the player weapons name.
    pub fn attach_paths(&self) -> BTreeSet<String> {
        self.weapons
            .values()
            .filter_map(|weapon| weapon.attach.clone())
            .collect()
    }

    /// The character definitions of the player weapons.
    pub fn definitions(&self) -> BTreeSet<String> {
        self.weapons
            .values()
            .flat_map(|weapon| weapon.definitions.iter().cloned())
            .collect()
    }
}

/// The weapon the attacks of `weapon` are cast on; `None` for no weapon (no equip type).
///
/// # Errors
///
/// Returns why when the rows' equip types sit on more than one attachment or a skeleton is
/// missing.
fn weapon_body<'a>(
    bodies: &'a Bodies,
    weapon: &WeaponStance,
) -> Result<Option<WeaponBody<'a>>, String> {
    let attachments: BTreeSet<&String> = weapon
        .equip_types
        .iter()
        .filter_map(|kind| bodies.equip_attachments.get(kind))
        .collect();
    let mut attachments = attachments.into_iter();
    let (Some(attachment), None) = (attachments.next(), attachments.next()) else {
        return if weapon.equip_types.is_empty() {
            Ok(None)
        } else {
            Err(format!(
                "equip types {:?}: not one attachment",
                weapon.equip_types
            ))
        };
    };
    Ok(Some(WeaponBody {
        attachment,
        skeletons: bodies.weapon_skeletons(&weapon.definitions)?,
    }))
}

/// The hit of the fragment of `action` for `tags`, with its damage row, its fragment's length
/// and, for a melee attack, its shapes cast on the weapon `body` (no links: [`links_of`] adds
/// them).
fn attack_of(
    sources: &AnimationSources,
    rows: &BundleRows,
    weapon: &WeaponStance,
    body: Result<&Option<WeaponBody<'_>>, &String>,
    tags: &[&str],
    action: &str,
) -> Result<BasicAttack, String> {
    let fragment_name = sources
        .fragment_of(action)
        .ok_or_else(|| format!("action {action}: no fragment in the action lists"))?;
    fragment_attack(sources, rows, weapon, body, tags, action, fragment_name)
}

/// The attack of fragment `fragment_name` that action `action` plays for `tags` ([`attack_of`]).
fn fragment_attack(
    sources: &AnimationSources,
    rows: &BundleRows,
    weapon: &WeaponStance,
    body: Result<&Option<WeaponBody<'_>>, &String>,
    tags: &[&str],
    action: &str,
    fragment_name: &str,
) -> Result<BasicAttack, String> {
    let player = sources.player_fragment_any_case(fragment_name, tags);
    let attach = weapon
        .attach
        .as_ref()
        .and_then(|path| sources.attach.get(path))
        .and_then(|adb| {
            let name = adb
                .fragments
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(fragment_name))?
                .name
                .clone();
            adb.best(&name, tags)
        });
    // The weapon's attach database first, then the player one.
    let fragments: Vec<(&Fragment, ClipContext)> = [
        attach.map(|fragment| (fragment, ClipContext::Weapon)),
        player.map(|fragment| (fragment, ClipContext::Character)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let playing_in = |kind: &'static str| -> (Vec<&ProcClip>, ClipContext) {
        fragments
            .iter()
            .map(|(fragment, context)| {
                let clips = fragment
                    .clips_of(kind)
                    .filter(|clip| sources.plays(clip, tags))
                    .collect::<Vec<_>>();
                (clips, *context)
            })
            .find(|(clips, _)| !clips.is_empty())
            .unwrap_or((Vec::new(), ClipContext::Character))
    };
    let playing = |kind: &'static str| playing_in(kind).0;
    let length_ms = player
        .map(|fragment| {
            sources
                .bodies
                .fragment_length_s(fragment)
                .map(ms)
                .ok_or_else(|| format!("{fragment_name}: an animation clip file is missing"))
        })
        .transpose()?
        .unwrap_or_default();
    // When a marker clip (`CAGE-Named`) of the fragment starts.
    let marker_start = |marker: &str| {
        fragments.iter().find_map(|(fragment, _)| {
            fragment
                .clips_of(NAMED_CLIP)
                .find(|clip| clip.param(NAMED_CLIP_NAME) == Some(marker))
                .map(|clip| clip.start_s)
        })
    };
    let can_transition_ms = marker_start(CAN_TRANSITION_MARKER).map(ms);
    let motion = match (
        sources
            .action(action)
            .and_then(|action| action.driven_motion.as_ref()),
        player.and_then(Fragment::main_animation),
    ) {
        (Some(driven), Some(body_clip)) => Some(
            attack_motion(sources, driven, body_clip, &fragments, tags, length_ms)
                .map_err(|why| format!("{fragment_name}: {why}"))?,
        ),
        _ => None,
    };
    let attack = |damage_row: &str, hit| BasicAttack {
        action: action.to_owned(),
        fragment: fragment_name.to_owned(),
        damage_row: damage_row.to_owned(),
        hit,
        length_ms,
        links: Vec::new(),
        can_transition_ms,
        motion: motion.clone(),
    };
    let (melee, context) = playing_in(DAMAGE_CLIP);
    if let Some(first) = melee.first() {
        let damage_row = first
            .param(DAMAGE_ROW)
            .ok_or_else(|| format!("{fragment_name}: damage clip without a row"))?;
        let start = melee
            .iter()
            .map(|clip| clip.start_s)
            .fold(f64::MAX, f64::min);
        let end = melee
            .iter()
            .map(|clip| clip.end_s)
            .try_fold(f64::MIN, |end, clip_end| {
                clip_end.map(|clip_end| end.max(clip_end))
            })
            .ok_or_else(|| format!("{fragment_name}: a damage clip lasts to the fragment's end"))?;
        let homing = player
            .and_then(|fragment| fragment.clips_of(HOMING_CLIP).next())
            .map(|clip| -> Result<Homing, String> {
                let value = |key| {
                    number(clip, key)
                        .ok_or_else(|| format!("{fragment_name}: homing without {key}"))
                };
                Ok(Homing {
                    radius_m: value(HOMING_RADIUS)?,
                    max_angle_deg: value(HOMING_MAX_ANGLE)?,
                    height_m: value(HOMING_HEIGHT)?,
                    radius_weight: value(HOMING_RADIUS_WEIGHT)?,
                    angle_weight: value(HOMING_ANGLE_WEIGHT)?,
                    use_radius_for_ai_target: clip.param(HOMING_AI_RADIUS) == Some(TRUE_TEXT),
                })
            })
            .transpose()?;
        let body_clip = player
            .and_then(Fragment::main_animation)
            .ok_or_else(|| format!("{fragment_name}: no animation for the shapes"))?;
        let weapon_clip = attach.and_then(Fragment::main_animation);
        let body = match (context, body) {
            (ClipContext::Weapon, Err(why)) => return Err(format!("{fragment_name}: {why}")),
            (_, Ok(body)) => body.as_ref(),
            (ClipContext::Character, Err(_)) => None,
        };
        let mut shapes = Vec::new();
        for clip in &melee {
            let shape = melee_shape(&sources.bodies, body, body_clip, weapon_clip, clip, context)
                .map_err(|why| format!("{fragment_name}: {why}"))?;
            shapes.extend(shape);
        }
        return Ok(attack(
            damage_row,
            AttackHit::Melee {
                window_ms: [ms(start), ms(end)],
                homing,
                shapes,
            },
        ));
    }
    let ranged_hit =
        |fire_ms: u64, (range_m, radius_m, hit_scan): (f32, f32, bool), prefab: Option<&String>| {
            if hit_scan {
                return Ok(AttackHit::HitScan { fire_ms, range_m });
            }
            let prefab = prefab.ok_or_else(|| format!("{fragment_name}: no projectile prefab"))?;
            let flight = sources
                .flights
                .get(&prefab.to_lowercase())
                .copied()
                .ok_or_else(|| format!("{fragment_name}: no flight in {prefab}"))?;
            Ok::<AttackHit, String>(AttackHit::Projectile {
                fire_ms,
                range_m,
                radius_m,
                flight,
            })
        };
    let ranged_attack = |profile: &String, name: &str| {
        sources
            .ranged
            .get(&(profile.clone(), name.to_owned()))
            .copied()
            .ok_or_else(|| format!("{fragment_name}: no ranged attack {profile}/{name}"))
    };
    if let Some(clip) = playing(RANGED_CLIP).first() {
        let profile = weapon.profile.as_ref().ok_or_else(|| {
            format!("{fragment_name}: ranged attack of a weapon without a profile")
        })?;
        let ranged = ranged_attack(profile, clip.param(RANGED_ATTACK_NAME).unwrap_or_default())?;
        let damage_row = clip
            .param(DAMAGE_ROW)
            .ok_or_else(|| format!("{fragment_name}: ranged clip without a row"))?;
        let hit = ranged_hit(ms(clip.start_s), ranged, weapon.ammo_prefab.as_ref())?;
        return Ok(attack(damage_row, hit));
    }
    // A cast clip, else a spell the action casts while a marker clip is on.
    let cast = playing(CAST_CLIP)
        .first()
        .map(|clip| (clip.start_s, clip.param(SPELL_NAME).unwrap_or_default()))
        .or_else(|| {
            sources
                .action(action)?
                .casts
                .iter()
                .find_map(|(condition, spell)| {
                    condition.markers().into_iter().find_map(|marker| {
                        let start_s = marker_start(marker)?;
                        condition
                            .holds(tags, &[marker])
                            .then_some((start_s, spell.as_str()))
                    })
                })
        });
    if let Some((start_s, spell)) = cast {
        let (damage_row, profile, name, prefab) = rows
            .spells
            .get(spell)
            .ok_or_else(|| format!("{fragment_name}: no projectile spell {spell}"))?;
        let hit = ranged_hit(ms(start_s), ranged_attack(profile, name)?, Some(prefab))?;
        return Ok(attack(damage_row, hit));
    }
    if fragments.is_empty() {
        return Err(format!("{fragment_name}: no entry for the stance"));
    }
    Err(format!("{fragment_name}: no damage, ranged or cast clip"))
}

/// How an attack of the action motion `driven` moves its player for `tags` ([`AttackMotion`]):
/// the root path of `body_clip` over the fragment's `length_ms`, each step times the multiplier
/// of the row that the markers of `fragments` on at the step's start pick (1 where no row sets
/// one); the velocity it keeps as the row at the start says (no fall where no row sets one).
///
/// # Errors
///
/// Returns why when the body's clip file is missing or has no frame time.
fn attack_motion(
    sources: &AnimationSources,
    driven: &DrivenMotion,
    body_clip: &AnimClip,
    fragments: &[(&Fragment, ClipContext)],
    tags: &[&str],
    length_ms: u64,
) -> Result<AttackMotion, String> {
    let condition = |name: &str| sources.actions.iter().find_map(|list| list.condition(name));
    let row_at = |time_s: f64| {
        let markers: Vec<&str> = fragments
            .iter()
            .flat_map(|(fragment, _)| fragment.clips_of(NAMED_CLIP))
            .filter(|clip| clip.start_s <= time_s && clip.end_s.is_none_or(|end| time_s < end))
            .filter_map(|clip| clip.param(NAMED_CLIP_NAME))
            .collect();
        driven.row(condition, tags, &markers)
    };
    // A fragment is a few seconds long.
    #[allow(clippy::cast_precision_loss)]
    let length_s = length_ms as f64 / MS_PER_SECOND;
    let (step_s, path) = root_path(&sources.bodies, body_clip, length_s)?;
    let mut offsets = Vec::with_capacity(path.len());
    let mut offset = [0.0_f32; 2];
    let mut last = [0.0_f32; 2];
    for &(time_s, root) in &path {
        let multiplier = row_at(time_s - step_s)
            .multiplier
            .unwrap_or(DEFAULT_MOTION_MULTIPLIER);
        offset = [
            offset[0] + (root[0] - last[0]) * multiplier,
            offset[1] + (root[1] - last[1]) * multiplier,
        ];
        last = root;
        offsets.push(offset);
    }
    let start = row_at(0.0);
    // A frame time is a few tens of milliseconds.
    #[allow(clippy::cast_possible_truncation)]
    let step_ms = (step_s * MS_PER_SECOND) as f32;
    Ok(AttackMotion {
        step_ms,
        offsets,
        inherit_velocity: start.inherit_velocity.unwrap_or(false),
        inherit_max_mps: start.inherit_max_mps,
        decel_mps2: start.decel_mps2.unwrap_or([0.0; 2]),
    })
}

/// The fragments that the instant transitions of `fragment` change to.
fn new_fragments(fragment: &Fragment) -> impl Iterator<Item = &str> {
    fragment
        .clips_of(TRANSITION_INSTANT)
        .filter_map(|clip| clip.param(NEW_FRAGMENT))
        .filter(|name| !name.is_empty())
}

/// The aimed shot and the drawn charge's shot of the stance of `tags` ([`Stance::aimed`],
/// [`Stance::aimed_charged`]): the entry of an aim action's fragment for the stance while it
/// aims (not the default entry), and its instant transitions to a fragment with an attack; a
/// transition to a fragment without one is a charge, whose own transitions give the charged
/// shot. A shot fragment with a ranged or cast clip that cannot be built goes to `skipped`.
fn aimed_of(
    sources: &AnimationSources,
    rows: &BundleRows,
    weapon: &WeaponStance,
    body: Result<&Option<WeaponBody<'_>>, &String>,
    tags: &[&str],
    skipped: &mut Vec<(String, String)>,
) -> (Option<BasicAttack>, Option<BasicAttack>) {
    let aiming: Vec<&str> = tags.iter().copied().chain([AIM_TAG]).collect();
    for action in AIM_ACTIONS {
        let Some(aim) = charge_fragment(sources, &aiming, action) else {
            continue;
        };
        let mut shot_of = |fragment: &str| {
            let shoots = sources
                .player_fragment_any_case(fragment, &aiming)
                .is_some_and(|entry| {
                    entry.clips_of(RANGED_CLIP).next().is_some()
                        || entry.clips_of(CAST_CLIP).next().is_some()
                });
            if !shoots {
                return None;
            }
            fragment_attack(sources, rows, weapon, body, &aiming, action, fragment)
                .map_err(|why| skipped.push((tags.join(","), why)))
                .ok()
        };
        let mut shot = None;
        let mut charges = Vec::new();
        for next in new_fragments(aim) {
            match shot_of(next) {
                Some(attack) => {
                    shot = shot.or(Some(attack));
                }
                None => charges.push(next),
            }
        }
        let charged = charges.into_iter().find_map(|charge| {
            let entry = sources.player_fragment_any_case(charge, &aiming)?;
            new_fragments(entry).find_map(&mut shot_of)
        });
        if shot.is_some() || charged.is_some() {
            return (shot, charged);
        }
    }
    (None, None)
}

/// One transition of a charge fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Transition<'a> {
    /// When its clip starts, ms after the press.
    start_ms: u64,
    /// Its named condition (empty for none).
    condition: &'a str,
    /// The action it starts.
    action: &'a str,
}

/// The charge fragment entry of action `action` for `tags` (not the default entry).
fn charge_fragment<'a>(
    sources: &'a AnimationSources,
    tags: &[&str],
    action: &str,
) -> Option<&'a Fragment> {
    let fragment = sources.fragment_of(action)?;
    sources
        .player_fragment_any_case(fragment, tags)
        .filter(|entry| !entry.tags.is_empty())
}

/// The instant transitions of `fragment`, by start.
fn transitions_of(fragment: &Fragment) -> Vec<Transition<'_>> {
    let mut transitions: Vec<Transition<'_>> = fragment
        .clips_of(TRANSITION_INSTANT)
        .map(|clip| Transition {
            start_ms: ms(clip.start_s),
            condition: clip.param(CONDITION).unwrap_or_default(),
            action: clip.param(NEW_ACTION).unwrap_or_default(),
        })
        .filter(|transition| !transition.action.is_empty())
        .collect();
    transitions.sort_by_key(|transition| transition.start_ms);
    transitions
}

/// Whether `action` is a charge action for `tags`: its fragment holds a release transition.
fn is_charge(sources: &AnimationSources, tags: &[&str], action: &str) -> bool {
    charge_fragment(sources, tags, action).is_some_and(|fragment| {
        transitions_of(fragment)
            .iter()
            .any(|transition| RELEASE_CONDITIONS.contains(&transition.condition))
    })
}

/// The light, heavy and charged transitions of a charge fragment's `transitions`: the first one
/// taken when the key is let go is the light attack, a later one with another action the heavy
/// attack, the first one taken while the key is held with another action the charged attack.
fn schedule_of<'t, 'a>(transitions: &'t [Transition<'a>]) -> [Option<&'t Transition<'a>>; 3] {
    let releases: Vec<&Transition> = transitions
        .iter()
        .filter(|transition| RELEASE_CONDITIONS.contains(&transition.condition))
        .collect();
    let light = releases.first().copied();
    let light_action = light.map(|transition| transition.action);
    let heavy = releases
        .iter()
        .find(|transition| Some(transition.action) != light_action)
        .copied();
    // A transition taken at the press holds nothing: a held one starts later.
    let charged = transitions.iter().find(|transition| {
        transition.start_ms > 0
            && HELD_CONDITIONS.contains(&transition.condition)
            && Some(transition.action) != light_action
    });
    [light, heavy, charged]
}

/// The combo links of `attack` for `tags`: its fragment's instant and buffered transitions to a
/// charge action, the step of each from `chain` (a new action joins its end). A buffered link
/// starts its step at the next instant clip of its layer to the same action.
fn links_of(
    sources: &AnimationSources,
    tags: &[&str],
    attack: &BasicAttack,
    chain: &mut Vec<String>,
) -> Result<Vec<ComboLink>, String> {
    let Some(fragment) = sources.player_fragment_any_case(&attack.fragment, tags) else {
        return Ok(Vec::new());
    };
    let mut links = Vec::new();
    for clip in fragment
        .clips
        .iter()
        .filter(|clip| clip.kind == TRANSITION_INSTANT || clip.kind == TRANSITION_BUFFERED)
    {
        let Some(action) = clip
            .param(NEW_ACTION)
            .filter(|action| is_charge(sources, tags, action))
        else {
            continue;
        };
        let starts_at_ms = if clip.kind == TRANSITION_BUFFERED {
            let instant = fragment
                .clips_of(TRANSITION_INSTANT)
                .filter(|other| {
                    other.layer == clip.layer
                        && other.start_s >= clip.start_s
                        && other.param(NEW_ACTION) == Some(action)
                })
                .map(|other| other.start_s)
                .fold(None, |first: Option<f64>, start| {
                    Some(first.map_or(start, |first| first.min(start)))
                })
                .ok_or_else(|| {
                    format!(
                        "{}: buffered link to {action} without an instant clip",
                        attack.fragment
                    )
                })?;
            Some(ms(instant))
        } else {
            None
        };
        let step = chain
            .iter()
            .position(|known| known == action)
            .unwrap_or_else(|| {
                chain.push(action.to_owned());
                chain.len() - 1
            });
        links.push(ComboLink {
            from_ms: ms(clip.start_s),
            until_ms: clip.end_s.map_or(attack.length_ms, ms),
            starts_at_ms,
            step,
        });
    }
    links.sort_by_key(|link| link.from_ms);
    Ok(links)
}

/// The stance of the weapons whose `MannequinTag` cell is `cell`, with the Mannequin tag `extra`
/// as well when given; the attacks it cannot build go to `skipped`.
fn stance_of(
    sources: &AnimationSources,
    rows: &BundleRows,
    cell: &str,
    extra: Option<&str>,
    weapon: &WeaponStance,
    skipped: &mut Vec<(String, String)>,
) -> Stance {
    let label = extra.map_or_else(|| cell.to_owned(), |tag| format!("{cell},{tag}"));
    let owned = stance_tags(&label);
    let tags: Vec<&str> = owned.iter().map(String::as_str).collect();
    let body = weapon_body(&sources.bodies, weapon);
    let mut chain: Vec<String> = CHARGE_ACTIONS
        .iter()
        .find(|action| charge_fragment(sources, &tags, action).is_some())
        .map(|action| (*action).to_owned())
        .into_iter()
        .collect();
    let mut steps: Vec<ChargeStep> = Vec::new();
    while let Some(charge_action) = chain.get(steps.len()).cloned() {
        let transitions = charge_fragment(sources, &tags, &charge_action)
            .map(transitions_of)
            .unwrap_or_default();
        let [light, heavy, charged] = schedule_of(&transitions);
        let mut build = |transition: Option<&Transition>| {
            let action = transition?.action;
            let built = attack_of(sources, rows, weapon, body.as_ref(), &tags, action).and_then(
                |mut attack| {
                    attack.links = links_of(sources, &tags, &attack, &mut chain)?;
                    Ok(attack)
                },
            );
            match built {
                Ok(attack) => Some(attack),
                Err(why) => {
                    skipped.push((label.clone(), why));
                    None
                }
            }
        };
        steps.push(ChargeStep {
            action: charge_action,
            light: build(light),
            light_from_ms: light.map_or(0, |transition| transition.start_ms),
            heavy_from_ms: heavy.map(|transition| transition.start_ms),
            heavy: build(heavy),
            charged_at_ms: charged.map(|transition| transition.start_ms),
            charged: build(charged),
        });
    }
    let dodge = sources
        .fragment_of(DODGE_ACTION)
        .and_then(|fragment| sources.player_fragment_any_case(fragment, &tags))
        .and_then(|fragment| fragment.clips_of(INVULNERABILITY_CLIP).next())
        .and_then(|clip| Some([ms(clip.start_s), ms(clip.end_s?)]));
    let uses = sources
        .fragment_of(USE_ACTION)
        .map(|fragment| {
            rows.use_tags
                .iter()
                .filter_map(|tag| {
                    let with_tag: Vec<&str> = tags.iter().copied().chain([tag.as_str()]).collect();
                    let entry = sources.player_fragment_any_case(fragment, &with_tag)?;
                    entry
                        .tags
                        .iter()
                        .any(|own| own.eq_ignore_ascii_case(tag))
                        .then(|| entry.clips_of(USE_CLIP).next())
                        .flatten()
                        .map(|clip| ItemUse {
                            tag: tag.clone(),
                            effect_ms: ms(clip.start_s),
                        })
                })
                .collect()
        })
        .unwrap_or_default();
    let sheathe_ms = sources.sheathe.as_ref().and_then(|sheathe| {
        let picked = sheathe.fragment_tags(&owned);
        let picked: Vec<&str> = picked.iter().map(String::as_str).collect();
        let entry = sources.player_fragment_any_case(&sheathe.fragment, &picked)?;
        sheathe.marker_clip(entry).map(|clip| ms(clip.start_s))
    });
    let (aimed, aimed_charged) = aimed_of(sources, rows, weapon, body.as_ref(), &tags, skipped);
    let mut steps = steps.into_iter();
    let first = steps.next();
    Stance {
        tags: label,
        light: first.as_ref().and_then(|step| step.light.clone()),
        light_from_ms: first.as_ref().map_or(0, |step| step.light_from_ms),
        heavy_from_ms: first.as_ref().and_then(|step| step.heavy_from_ms),
        heavy: first.as_ref().and_then(|step| step.heavy.clone()),
        charged_at_ms: first.as_ref().and_then(|step| step.charged_at_ms),
        charged: first.and_then(|step| step.charged),
        dodge_invulnerable_ms: dodge,
        sheathe_ms,
        uses,
        combo: steps.collect(),
        variants: Vec::new(),
        aimed,
        aimed_charged,
    }
}

/// The status effects that set a Mannequin tag on a character of `tags`, with the tag: actions
/// of the action lists with a `SetMannequinTag` activity active on a status effect whose other
/// tests hold for the character.
fn tag_setters(sources: &AnimationSources, tags: &[&str]) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = sources
        .actions
        .iter()
        .flat_map(|list| &list.actions)
        .filter_map(|action| {
            let (effect, rest) = action.on_status_effect.as_ref()?;
            rest.holds(tags, &[]).then_some((effect, &action.sets_tags))
        })
        .flat_map(|(effect, set)| {
            set.iter()
                .filter(|tag| !tags.iter().any(|own| own.eq_ignore_ascii_case(tag)))
                .map(move |tag| (effect.clone(), tag.clone()))
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The stance of `cell` with its variants.
fn stance_with_variants(
    sources: &AnimationSources,
    rows: &BundleRows,
    cell: &str,
    weapon: &WeaponStance,
    skipped: &mut Vec<(String, String)>,
) -> Stance {
    let mut stance = stance_of(sources, rows, cell, None, weapon, skipped);
    let owned = stance_tags(cell);
    let tags: Vec<&str> = owned.iter().map(String::as_str).collect();
    for (status_effect, tag) in tag_setters(sources, &tags) {
        let variant = stance_of(sources, rows, cell, Some(&tag), weapon, skipped);
        let same = Stance {
            tags: stance.tags.clone(),
            ..variant.clone()
        } == stance;
        if !same {
            stance.variants.push(StanceVariant {
                status_effect,
                tag,
                stance: variant,
            });
        }
    }
    stance
}

/// The stances of every player weapon `MannequinTag` cell of `rows`, and of no weapon.
pub fn build_stances(
    sources: &AnimationSources,
    rows: &BundleRows,
) -> (Vec<Stance>, PlayerAttacksReport) {
    let mut report = PlayerAttacksReport::default();
    let no_weapon = (String::new(), WeaponStance::default());
    let stances: Vec<Stance> = std::iter::once((&no_weapon.0, &no_weapon.1))
        .chain(rows.weapons.iter().filter(|(cell, _)| !cell.is_empty()))
        .map(|(cell, weapon)| {
            stance_with_variants(sources, rows, cell, weapon, &mut report.skipped)
        })
        .collect();
    report.stances = stances.len();
    report.skipped.sort();
    report.skipped.dedup();
    (stances, report)
}

/// The ObjectStream root of a pak entry, AZCS or not.
fn stream_root(bytes: &[u8]) -> Option<Element> {
    let decoded;
    let bytes = if is_azcs(bytes) {
        decoded = decode_azcs(bytes).ok()?;
        &decoded
    } else {
        bytes
    };
    parse_object_stream(bytes).ok()
}

/// The animation names of the clips of `databases`.
fn clip_names<'a>(databases: impl Iterator<Item = &'a AnimDatabase>) -> BTreeSet<String> {
    databases
        .flat_map(|adb| &adb.fragments)
        .flat_map(|fragment| fragment.anim_layers.iter().flatten())
        .map(|clip| clip.name.to_lowercase())
        .collect()
}

/// The animation name of the clip file at `path` (lower case) under the clip folders.
fn clip_name(path: &str) -> Option<&str> {
    CLIP_FOLDERS
        .iter()
        .any(|folder| path.starts_with(folder))
        .then(|| path.strip_suffix(CLIP_SUFFIX))
        .flatten()
        .and_then(|stem| stem.rsplit('/').next())
}

/// Reads the animation sources of `rows` from `source`: a first walk for the databases, action
/// lists, ranged attacks, projectile slices, character definitions and equip types, a second for
/// the skeletons and the clips they name.
///
/// # Errors
///
/// Returns [`ToolError`] when the paks cannot be read.
pub fn read_sources(source: &GameSource, rows: &BundleRows) -> Result<AnimationSources, ToolError> {
    let attach_paths = rows.attach_paths();
    let definitions = rows.definitions();
    let prefab_slices: HashMap<String, String> = rows
        .prefabs()
        .into_iter()
        .map(|prefab| (slice_path(&prefab), prefab.to_lowercase()))
        .collect();
    let mut sources = AnimationSources {
        player: Vec::new(),
        attach: HashMap::new(),
        actions: Vec::new(),
        sheathe: None,
        ranged: HashMap::new(),
        flights: HashMap::new(),
        bodies: Bodies::default(),
    };
    source.for_each_entry(
        |path| {
            (path.starts_with(PLAYER_ADB_PREFIX) && path.ends_with(ADB_SUFFIX))
                || attach_paths.contains(path)
                || (path.starts_with(PLAYER_ACTIONS_PREFIX) && path.ends_with(ACTION_LIST_SUFFIX))
                || path == RANGED_ATTACK_DATABASE
                || prefab_slices.contains_key(path)
                || path == PLAYER_DEFINITION
                || path == EQUIP_TYPES
                || definitions.contains(path)
        },
        |path, bytes| {
            let text = || std::str::from_utf8(&bytes).ok();
            if path == RANGED_ATTACK_DATABASE {
                if let Some(root) = stream_root(&bytes) {
                    sources.ranged = ranged_attacks(&root);
                }
            } else if path == EQUIP_TYPES {
                if let Some(root) = stream_root(&bytes) {
                    sources.bodies.equip_attachments = equip_attachments(&root);
                }
            } else if let Some(prefab) = prefab_slices.get(path) {
                if let Some(flight) = stream_root(&bytes).as_ref().and_then(slice_flight) {
                    sources.flights.insert(prefab.clone(), flight);
                }
            } else if path == PLAYER_DEFINITION || definitions.contains(path) {
                if let Some(definition) =
                    text().and_then(|text| CharacterDefinition::parse(text).ok())
                {
                    if path == PLAYER_DEFINITION {
                        sources.bodies.player_definition = definition;
                    } else {
                        sources
                            .bodies
                            .weapon_definitions
                            .insert(path.to_owned(), definition);
                    }
                }
            } else if path.ends_with(ACTION_LIST_SUFFIX) {
                if let Some(list) = text().and_then(|text| ActionList::parse(text).ok()) {
                    sources.actions.push(list);
                }
                if sources.sheathe.is_none() {
                    sources.sheathe = text()
                        .and_then(|text| SheatheAction::parse(text, SHEATHE_ACTION).ok())
                        .flatten();
                }
            } else if let Some(adb) = text().and_then(|text| AnimDatabase::parse(text).ok()) {
                if attach_paths.contains(path) {
                    sources.attach.insert(path.to_owned(), adb);
                } else {
                    sources.player.push(adb);
                }
            }
            Ok::<(), ToolError>(())
        },
    )?;
    let names = clip_names(sources.player.iter().chain(sources.attach.values()));
    let skeletons: BTreeSet<String> = std::iter::once(PLAYER_SKELETON.to_owned())
        .chain(
            sources
                .bodies
                .weapon_definitions
                .values()
                .map(|definition| normalized(&definition.model)),
        )
        .collect();
    let bodies = &mut sources.bodies;
    source.for_each_entry(
        |path| skeletons.contains(path) || clip_name(path).is_some_and(|name| names.contains(name)),
        |path, bytes| {
            if skeletons.contains(path) {
                if let Ok(skeleton) = Skeleton::parse(&bytes) {
                    if path == PLAYER_SKELETON {
                        bodies.player = skeleton.clone();
                    }
                    bodies.skeletons.insert(path.to_owned(), skeleton);
                }
            } else if let Some(name) = clip_name(path) {
                // Two files of one name: the name is not known for sure.
                let clip = Caf::parse(&bytes).ok();
                bodies
                    .clips
                    .entry(name.to_owned())
                    .and_modify(|known| *known = None)
                    .or_insert(clip);
            }
            Ok::<(), ToolError>(())
        },
    )?;
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use nw_formats::objectstream::write_object_stream;

    use super::*;

    /// A player database cut: the sword's charge, light and heavy fragments, the dodge and the
    /// uses (`player_sword.adb` and `player.adb`, build 25810808), with a perk-only clip, the
    /// combo links of the light attacks (`Attack_Primary` to `Attack_Start_Charge_C2`, buffered
    /// from 333 ms to the instant clip at 533 ms), the light attack's translation marker for
    /// its first half second, and a stance whose `GenericTagA` charges sooner.
    const PLAYER_ADB: &str = r#"<AnimDB><FragmentList>
 <Attack_Start_Charge><Fragment Tags="1H_Melee+Sword"><ProcLayer>
  <Blend ExitTime="0"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="Attack_Heavy_NotHeld"/><NewAction value="Attack_Primary_C1"/></ProceduralParams></Procedural>
  <Blend ExitTime="0.33333334"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="Attack_Primary_Released"/><NewAction value="Attack_Heavy"/></ProceduralParams></Procedural>
  <Blend ExitTime="0.33333334"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value=""/><NewAction value="Attack_Heavy_Charged"/></ProceduralParams></Procedural>
 </ProcLayer><ProcLayer>
  <Blend ExitTime="0.16666667"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="CanAttack_SpinAttack"/><NewAction value="Sword_SpinAttack"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment>
 <Fragment Tags="1H_Melee+Sword+GenericTagA"><ProcLayer>
  <Blend ExitTime="0"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="Attack_Heavy_NotHeld"/><NewAction value="Attack_Primary_C1"/></ProceduralParams></Procedural>
  <Blend ExitTime="0.5"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value=""/><NewAction value="Attack_Heavy_Charged"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Attack_Start_Charge>
 <Attack_Start_Charge_C2><Fragment Tags="1H_Melee+Sword"><ProcLayer>
  <Blend ExitTime="0"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="Attack_Heavy_NotHeld"/><NewAction value="Attack_Primary_C2"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Attack_Start_Charge_C2>
 <Attack_Primary><Fragment Tags="1H_Melee+Sword"><AnimLayer>
  <Blend ExitTime="0" StartTime="0" Duration="0.2"/><Animation name="swing"/>
 </AnimLayer><ProcLayer>
  <Blend ExitTime="0.1"/><Procedural type="Homing"><ProceduralParams><Radius value="3.75"/><MaxAngle value="60"/><Height value="5"/><RadiusWeight value="1"/><AngleWeight value="1"/><UseRadiusforAITarget value="true"/></ProceduralParams></Procedural>
  <Blend ExitTime="0.3"/><Procedural type=""/>
 </ProcLayer><ProcLayer>
  <Blend ExitTime="0.76666665"/><Procedural type="CAGE-Named"><ProceduralParams><Name value="CanTransition"/></ProceduralParams></Procedural>
 </ProcLayer><ProcLayer>
  <Blend ExitTime="0"/><Procedural type="CAGE-Named"><ProceduralParams><Name value="Attack_Translation"/></ProceduralParams></Procedural>
  <Blend ExitTime="0.5"/><Procedural type=""/>
 </ProcLayer><ProcLayer>
  <Blend ExitTime="0.33333334"/><Procedural type="CAGE-TransitionBuffered"><ProceduralParams><Condition value="CanAttack_Heavy"/><NewAction value="Attack_Start_Charge_C2"/></ProceduralParams></Procedural>
  <Blend ExitTime="0.2"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="CanAttack_Heavy"/><NewAction value="Attack_Start_Charge_C2"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Attack_Primary>
 <Attack_Primary_Strike2><Fragment Tags="1H_Melee+Sword"><AnimLayer>
  <Blend ExitTime="0" StartTime="0.5" Duration="0.2"/><Animation name="swing" speed="2"/>
 </AnimLayer><ProcLayer>
  <Blend ExitTime="0.1"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="CanAttack_Heavy"/><NewAction value="Attack_Start_Charge"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Attack_Primary_Strike2>
 <Attack_Heavy_Charged><Fragment Tags="1H_Melee+Sword"><ProcLayer/></Fragment></Attack_Heavy_Charged>
 <Dodge_Roll><Fragment Tags=""><ProcLayer>
  <Blend ExitTime="0"/><Procedural type="Invulnerability"><ProceduralParams/></Procedural>
  <Blend ExitTime="0.5"/><Procedural type=""/>
 </ProcLayer></Fragment></Dodge_Roll>
 <Use><Fragment Tags="Potion"><ProcLayer>
  <Blend ExitTime="0.33333334"/><Procedural type="UsePaperdollItem"><ProceduralParams/></Procedural>
  <Blend ExitTime="0.3"/><Procedural type=""/>
 </ProcLayer></Fragment><Fragment Tags="Crouch+Potion"><ProcLayer>
  <Blend ExitTime="0.8333333"/><Procedural type="UsePaperdollItem"><ProceduralParams/></Procedural>
 </ProcLayer></Fragment></Use>
 <Sheathe_UpperBody><Fragment Tags="From_1H_Melee_Sword"><ProcLayer>
  <Blend ExitTime="0.40000001"/><Procedural type="CAGE-Named"><ProceduralParams><Name value="sheathe_weapon"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment><Fragment Tags="Sword+Crouch+From_1H_Melee"><ProcLayer>
  <Blend ExitTime="0.40000001"/><Procedural type="CAGE-Named"><ProceduralParams><Name value="sheathe_weapon"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Sheathe_UpperBody>
</FragmentList></AnimDB>"#;

    /// The sword's attach database cut (`playermaleanims_1h_meleeattach.adb`): the light
    /// attack's capsule on the weapon, 333-433 ms, the second light attack's, and a heavy whose
    /// clip lasts to the fragment's end.
    const ATTACH_ADB: &str = r#"<AnimDB><FragmentList>
 <Attack_Primary><Fragment Tags="1H_Melee+Sword"><ProcLayer>
  <Blend ExitTime="0.33333334"/><Procedural type="CAGE-Damage"><ProceduralParams><DamageTableRow value="1H_Sword_Attack1"/><OffsetX value="0"/><OffsetY value="0"/><OffsetZ value="0.5"/><MeleeAttackShapeCastType value="MeleeAttackShapeCastTypeCapsule"/><MeleeAttackShapeRadius value="0.3"/><MeleeAttackCapsuleHalfHeight value="0.3"/><JointName value=""/><xRotationOffset value="0"/><yRotationOffset value="0"/><zRotationOffset value="0"/><Condition value=""/></ProceduralParams></Procedural>
  <Blend ExitTime="0.1"/><Procedural type=""/>
 </ProcLayer><ProcLayer>
  <Blend ExitTime="0.2"/><Procedural type="CAGE-Damage"><ProceduralParams><DamageTableRow value="Perk_Row"/><Condition value="Some_Perk"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Attack_Primary>
 <Attack_Primary_Strike2><Fragment Tags="1H_Melee+Sword"><ProcLayer>
  <Blend ExitTime="0.1"/><Procedural type="CAGE-Damage"><ProceduralParams><DamageTableRow value="1H_Sword_Attack1"/><OffsetX value="0"/><OffsetY value="0"/><OffsetZ value="0"/><MeleeAttackShapeCastType value="MeleeAttackShapeCastTypeSphere"/><MeleeAttackShapeRadius value="0.5"/><JointName value="master_root"/><xRotationOffset value="0"/><yRotationOffset value="0"/><zRotationOffset value="0"/></ProceduralParams></Procedural>
  <Blend ExitTime="0.1"/><Procedural type=""/>
 </ProcLayer></Fragment></Attack_Primary_Strike2>
 <Attack_Heavy><Fragment Tags="1H_Melee+Sword"><ProcLayer>
  <Blend ExitTime="0.2"/><Procedural type="CAGE-Damage"><ProceduralParams><DamageTableRow value="1H_Sword_Attack2"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Attack_Heavy>
</FragmentList></AnimDB>"#;

    const ACTIONS: &str = r#"<ActionList><Actions>
 <Action Name="Attack_Start_Charge" FragmentName="Attack_Start_Charge"/>
 <Action Name="Attack_Start_Charge_C2" FragmentName="Attack_Start_Charge_C2"/>
 <Action Name="Attack_Primary_C1" FragmentName="Attack_Primary"><Activities><AnimationDrivenMotion>
  <Default MotionMultiplier="1.00" Decel="9,9,9" InheritVelocity="false"/>
  <Conditional Name="Attack_Translation_Sword" MotionMultiplier="2.0" InheritVelocity="true" InheritVelocityMaxSpeed="6"/>
 </AnimationDrivenMotion></Activities></Action>
 <Action Name="Attack_Primary_C2" FragmentName="Attack_Primary_Strike2"/>
 <Action Name="Attack_Heavy" FragmentName="Attack_Heavy"/>
 <Action Name="Attack_Heavy_Charged" FragmentName="Attack_Heavy_Charged"/>
 <Action Name="Dodge" FragmentName="Dodge_Roll"/>
 <Action Name="Use_QuickSlot_1" FragmentName="Use"/>
 <Action Name="Sheathe_MainHand_1_UB" FragmentName="Sheathe_UpperBody"><Activities>
  <IfThenElse><IfCondition><IfMannequinMarker MarkerName="sheathe_weapon"/></IfCondition>
   <ThenActivity><SheatheWeapon><Default SheatheOption="Sheathe" SlotToSheathe="MAIN_HAND_OPTION_1"/></SheatheWeapon></ThenActivity></IfThenElse>
  <SetMannequinTag><Default TagName="From_Unarmed" OnEnter="Set"/><Conditional Name="1H_Melee_Sword" TagName="From_1H_Melee_Sword" OnEnter="Set"/></SetMannequinTag>
  <SetMannequinTagFromSource><Default TagSource="PaperdollSlot" SourceSlot="MAIN_HAND_OPTION_1" OnEnter="Unset"/></SetMannequinTagFromSource>
 </Activities></Action>
 <Action Name="Sword_OffenseStance">
  <ActivationCondition><And><IfHasStatusEffect StatusEffect="Sword_Stance_Offense"/><IfMannequinTag TagName="Sword"/></And></ActivationCondition>
  <Activities><SetMannequinTag><Default TagName="GenericTagA" OnEnter="Set" OnExit="Unset"/></SetMannequinTag></Activities>
 </Action>
 <Action Name="Bow_Stance">
  <ActivationCondition><And><IfHasStatusEffect StatusEffect="Bow_Stance"/><IfMannequinTag TagName="Bow"/></And></ActivationCondition>
  <Activities><SetMannequinTag><Default TagName="GenericTagB" OnEnter="Set" OnExit="Unset"/></SetMannequinTag></Activities>
 </Action>
</Actions>
<Conditions><And Name="1H_Melee_Sword"><IfMannequinTag TagName="1H_Melee"/><IfMannequinTag TagName="Sword"/></And>
<And Name="Attack_Translation_Sword"><IfMannequinMarker MarkerName="Attack_Translation"/><IfMannequinTag TagName="Sword"/></And></Conditions>
</ActionList>"#;

    const ATTACH: &str = "animations/mannequin/adb/playermaleanims_1h_meleeattach.adb";
    const SWORD: &str = "1H_Melee,Sword";

    fn sources() -> AnimationSources {
        AnimationSources {
            player: vec![AnimDatabase::parse(PLAYER_ADB).expect("player")],
            attach: HashMap::from([(
                ATTACH.to_owned(),
                AnimDatabase::parse(ATTACH_ADB).expect("attach"),
            )]),
            actions: vec![ActionList::parse(ACTIONS).expect("actions")],
            sheathe: SheatheAction::parse(ACTIONS, SHEATHE_ACTION).expect("actions"),
            ranged: HashMap::new(),
            flights: HashMap::new(),
            bodies: crate::melee_shapes::test_support::bodies(),
        }
    }

    fn rows() -> BundleRows {
        use crate::melee_shapes::test_support::{SWORD_DEFINITION, SWORD_EQUIP_TYPE};
        BundleRows {
            weapons: BTreeMap::from([(
                SWORD.to_owned(),
                WeaponStance {
                    attach: Some(ATTACH.to_owned()),
                    profile: None,
                    ammo_prefab: None,
                    equip_types: BTreeSet::from([SWORD_EQUIP_TYPE.to_owned()]),
                    definitions: BTreeSet::from([SWORD_DEFINITION.to_owned()]),
                },
            )]),
            spells: HashMap::new(),
            use_tags: BTreeSet::from(["Potion".to_owned()]),
        }
    }

    fn sword(stances: &[Stance]) -> &Stance {
        stances
            .iter()
            .find(|stance| stance.tags == SWORD)
            .expect("sword")
    }

    #[test]
    fn an_attack_moves_by_its_root_times_the_multiplier_of_its_markers() {
        const CLOSE: f32 = 1e-4;
        let (stances, _) = build_stances(&sources(), &rows());
        let sword = sword(&stances);
        let motion = sword
            .light
            .as_ref()
            .and_then(|light| light.motion.clone())
            .expect("the light attack's driven motion");
        // The root goes 1 m forward over the clip's 30 frames; the marker doubles its first
        // half second.
        assert!((motion.step_ms - 1_000.0 / 30.0).abs() < CLOSE);
        assert_eq!(motion.offsets.len(), 31, "from the start to the end");
        let [right, forward] = motion.offset_at(1_000.0);
        assert!(
            right.abs() < CLOSE && (forward - 1.5).abs() < CLOSE,
            "{forward}"
        );
        assert!((motion.offset_at(500.0)[1] - 1.0).abs() < CLOSE);
        assert!(motion.inherit_velocity, "the marker's row at the start");
        assert_eq!(motion.inherit_max_mps, Some(6.0));
        assert_eq!(motion.decel_mps2, [9.0, 9.0], "the default's");
        let second = sword
            .schedule(1)
            .and_then(|step| step.attack(nw_content::player_attacks::AttackKind::Light));
        assert_eq!(
            second.map(|attack| attack.motion.is_some()),
            Some(false),
            "an action without a driven motion"
        );
    }

    #[test]
    fn a_stance_has_its_charge_schedule_attacks_dodge_and_uses() {
        let (stances, report) = build_stances(&sources(), &rows());
        assert_eq!(stances.len(), 2, "no weapon and the sword");
        let sword = sword(&stances);
        assert_eq!(
            (
                sword.light_from_ms,
                sword.heavy_from_ms,
                sword.charged_at_ms
            ),
            (0, Some(333), Some(667))
        );
        let light = sword.light.as_ref().expect("light");
        assert_eq!(light.damage_row, "1H_Sword_Attack1", "not the perk clip");
        let AttackHit::Melee {
            window_ms,
            homing,
            shapes,
        } = &light.hit
        else {
            panic!("melee");
        };
        assert_eq!(*window_ms, [333, 433]);
        let homing = homing.expect("homing");
        assert_eq!(
            (homing.radius_m, homing.max_angle_deg, homing.height_m),
            (3.75, 60.0, 5.0)
        );
        assert!(homing.use_radius_for_ai_target);
        assert_eq!(shapes.len(), 1, "not the perk clip");
        assert_eq!(shapes[0].window_ms, [333, 433]);
        assert_eq!(light.length_ms, 1_000, "the clip's 30 frames at its speed");
        assert_eq!(
            light.can_transition_ms,
            Some(767),
            "its CanTransition marker"
        );
        assert_eq!(sword.heavy, None, "its clip has no end");
        assert_eq!(sword.charged, None, "no clip at all");
        assert_eq!(sword.dodge_invulnerable_ms, Some([0, 500]));
        assert_eq!(
            sword.sheathe_ms,
            Some(400),
            "the sheathe marker of From_1H_Melee_Sword: the sword's own tags are cleared"
        );
        assert_eq!(
            sword.use_effect_ms("Potion"),
            Some(333),
            "not the crouch use"
        );
        assert!(
            report
                .skipped
                .iter()
                .any(|(cell, why)| cell == SWORD && why.contains("fragment's end")),
            "{report:?}"
        );
    }

    #[test]
    fn a_light_attack_links_to_the_next_combo_step_and_back() {
        let (stances, _) = build_stances(&sources(), &rows());
        let sword = sword(&stances);
        let light = sword.light.as_ref().expect("light");
        assert_eq!(
            light.links,
            [
                ComboLink {
                    from_ms: 333,
                    until_ms: 533,
                    starts_at_ms: Some(533),
                    step: 1,
                },
                ComboLink {
                    from_ms: 533,
                    until_ms: 1_000,
                    starts_at_ms: None,
                    step: 1,
                },
            ]
        );
        assert_eq!(sword.combo.len(), 1);
        let second = &sword.combo[0];
        assert_eq!(second.action, "Attack_Start_Charge_C2");
        let light_2 = second.light.as_ref().expect("second light");
        assert_eq!(light_2.action, "Attack_Primary_C2");
        // (1 s - 0.5 s) / 2.
        assert_eq!(light_2.length_ms, 250);
        assert_eq!(light_2.can_transition_ms, None, "no CanTransition marker");
        assert_eq!(light_2.links[0].step, 0, "back to the stance's own step");
        let AttackHit::Melee { shapes, .. } = &light_2.hit else {
            panic!("melee");
        };
        assert_eq!(shapes.len(), 1);
    }

    #[test]
    fn a_status_effect_that_sets_a_tag_gives_a_stance_variant() {
        let (stances, _) = build_stances(&sources(), &rows());
        let sword = sword(&stances);
        assert_eq!(sword.variants.len(), 1, "not the bow's stance");
        let variant = &sword.variants[0];
        assert_eq!(
            (variant.status_effect.as_str(), variant.tag.as_str()),
            ("Sword_Stance_Offense", "GenericTagA")
        );
        assert_eq!(variant.stance.tags, "1H_Melee,Sword,GenericTagA");
        assert_eq!(variant.stance.charged_at_ms, Some(500));
        assert_eq!(
            sword
                .with_status_effects(&["Sword_Stance_Offense"])
                .charged_at_ms,
            Some(500)
        );
    }

    /// The aim fragments of `player_2h_ranged.adb` and `player_bow.adb` (build 25810808), cut:
    /// the musket's aim changes to its aimed shot (`MusketAttack1` at 0.033 s) or lowers the
    /// weapon; the bow's aim shoots at once (`BowAttack1`) or draws a charge whose release
    /// shoots the charged arrow (`BowAttack2`).
    const AIM_ADB: &str = r#"<AnimDB><FragmentList>
 <Firearm_Aim_UpperBody><Fragment Tags="2H_Ranged"><ProcLayer>
  <Blend ExitTime="0"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="Aim_Shoot"/><NewFragment value="Firearm_Shoot_Primary_UpperBody"/><NewAction value=""/></ProceduralParams></Procedural>
  <Blend ExitTime="0"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="Aim_Exit"/><NewFragment value="Firearm_LowerWeapon_UpperBody"/><NewAction value=""/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Firearm_Aim_UpperBody>
 <Firearm_Shoot_Primary_UpperBody><Fragment Tags="2H_Ranged+Aim"><ProcLayer>
  <Blend ExitTime="0.033333335"/><Procedural type="CAGE-RangedAttack"><ProceduralParams><RangedAttackName value="Basic"/><DamageTableRow value="MusketAttack1"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Firearm_Shoot_Primary_UpperBody>
 <Firearm_Shoot_HipFire><Fragment Tags="2H_Ranged"><ProcLayer>
  <Blend ExitTime="0.2"/><Procedural type="CAGE-RangedAttack"><ProceduralParams><RangedAttackName value="Basic"/><DamageTableRow value="MusketAttack_Hipfire"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Firearm_Shoot_HipFire>
 <Firearm_LowerWeapon_UpperBody><Fragment Tags="2H_Ranged"><ProcLayer/></Fragment></Firearm_LowerWeapon_UpperBody>
 <Bow_Aim_UpperBody><Fragment Tags="Bow"><ProcLayer>
  <Blend ExitTime="0"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="Charge_Bow"/><NewFragment value="Bow_Charge_UpperBody"/><NewAction value=""/></ProceduralParams></Procedural>
  <Blend ExitTime="0"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="Shoot_NoCharge_Bow"/><NewFragment value="Bow_Shoot_UpperBody"/><NewAction value=""/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Bow_Aim_UpperBody>
 <Bow_Charge_UpperBody><Fragment Tags="Bow"><ProcLayer>
  <Blend ExitTime="0"/><Procedural type="CAGE-TransitionInstant"><ProceduralParams><Condition value="Charge_Release_Bow"/><NewFragment value="Bow_Shoot_Charged"/><NewAction value=""/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Bow_Charge_UpperBody>
 <Bow_Shoot_UpperBody><Fragment Tags="Bow"><ProcLayer>
  <Blend ExitTime="0.033333335"/><Procedural type="CAGE-RangedAttack"><ProceduralParams><RangedAttackName value="Basic"/><DamageTableRow value="BowAttack1"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Bow_Shoot_UpperBody>
 <Bow_Shoot_Charged><Fragment Tags="Bow"><ProcLayer>
  <Blend ExitTime="0"/><Procedural type="CAGE-RangedAttack"><ProceduralParams><RangedAttackName value="Basic"/><DamageTableRow value="BowAttack2"/></ProceduralParams></Procedural>
 </ProcLayer></Fragment></Bow_Shoot_Charged>
</FragmentList></AnimDB>"#;

    const AIM_ACTION_LIST: &str = r#"<ActionList><Actions>
 <Action Name="Firearm_AimShoot_SubState" FragmentName="Firearm_Aim_UpperBody"/>
 <Action Name="Bow_AimChargeShoot_SubState" FragmentName="Bow_Aim_UpperBody"/>
</Actions></ActionList>"#;

    #[test]
    fn a_gun_and_a_bow_fire_the_aimed_shots_of_their_aim_fragments() {
        const MUSKET: &str = "2H_Ranged,Rifle";
        const BOW: &str = "Bow,LongBow";
        /// The hit scan of the test profiles: 200 m.
        const RANGE_M: f32 = 200.0;
        let weapon = |profile: &str| WeaponStance {
            attach: None,
            profile: Some(profile.to_owned()),
            ammo_prefab: None,
            equip_types: BTreeSet::new(),
            definitions: BTreeSet::new(),
        };
        let sources = AnimationSources {
            player: vec![AnimDatabase::parse(AIM_ADB).expect("player")],
            attach: HashMap::new(),
            actions: vec![ActionList::parse(AIM_ACTION_LIST).expect("actions")],
            sheathe: None,
            ranged: HashMap::from([
                (
                    ("Musket".to_owned(), "Basic".to_owned()),
                    (RANGE_M, 0.0, true),
                ),
                (("Bow".to_owned(), "Basic".to_owned()), (RANGE_M, 0.0, true)),
            ]),
            flights: HashMap::new(),
            bodies: crate::melee_shapes::test_support::bodies(),
        };
        let rows = BundleRows {
            weapons: BTreeMap::from([
                (MUSKET.to_owned(), weapon("Musket")),
                (BOW.to_owned(), weapon("Bow")),
            ]),
            spells: HashMap::new(),
            use_tags: BTreeSet::new(),
        };
        let (stances, _) = build_stances(&sources, &rows);
        let of = |tags: &str| {
            stances
                .iter()
                .find(|stance| stance.tags == tags)
                .expect("stance")
        };
        let musket = of(MUSKET);
        let aimed = musket.aimed.as_ref().expect("the aimed shot");
        assert_eq!(aimed.damage_row, "MusketAttack1", "not the hip fire's row");
        assert_eq!(aimed.fragment, "Firearm_Shoot_Primary_UpperBody");
        assert_eq!(
            aimed.hit,
            AttackHit::HitScan {
                fire_ms: 33,
                range_m: RANGE_M
            }
        );
        assert_eq!(musket.aimed_charged, None, "lowering the gun is no charge");
        let bow = of(BOW);
        assert_eq!(
            bow.aimed.as_ref().map(|shot| shot.damage_row.as_str()),
            Some("BowAttack1")
        );
        assert_eq!(
            bow.aimed_charged
                .as_ref()
                .map(|shot| shot.damage_row.as_str()),
            Some("BowAttack2"),
            "the charge's release"
        );
        assert_eq!(of("").aimed, None, "no weapon aims");
    }

    #[test]
    fn the_ranged_database_and_a_slice_give_range_radius_and_flight() {
        let crc = lowercase_crc32;
        let float =
            |name: &str, value: f32| Element::with_value(crc(name), value.to_be_bytes().to_vec());
        let text =
            |name: &str, value: &str| Element::with_value(crc(name), value.as_bytes().to_vec());
        // `Bow` / `Basic` of build 25810808: 100 m, radius 0.15, no hit scan.
        let attack = Element::with_children(
            0,
            vec![
                text(KEY_FIELD, "Basic"),
                Element::with_children(
                    crc(VALUE_FIELD),
                    vec![
                        Element::with_value(crc(HIT_SCAN_FIELD), vec![0]),
                        float(MAX_RANGE_FIELD, 100.0),
                        float(RADIUS_FIELD, 0.15),
                    ],
                ),
            ],
        );
        let profile = Element::with_children(
            0,
            vec![
                text(KEY_FIELD, "Bow"),
                Element::with_children(
                    crc(VALUE_FIELD),
                    vec![Element::with_children(crc(ATTACKS_FIELD), vec![attack])],
                ),
            ],
        );
        let root = Element::with_children(
            0,
            vec![Element::with_children(crc(PROFILES_FIELD), vec![profile])],
        );
        let bytes = write_object_stream(&root).expect("stream");
        let ranged = ranged_attacks(&stream_root(&bytes).expect("root"));
        assert_eq!(
            ranged.get(&("Bow".to_owned(), "Basic".to_owned())),
            Some(&(100.0, 0.15, false))
        );
        // `basic_arrow.dynamicslice`: 100 m/s, 25 more over 1 s, `m_accel` (0, 0, -9.8), 10 s.
        let accel: Vec<u8> = [0.0_f32, 0.0, -9.8]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let slice = Element::with_children(
            0,
            vec![Element::with_children(
                0,
                vec![
                    float(BASE_SPEED_FIELD, 100.0),
                    float(EXTRA_SPEED_FIELD, 25.0),
                    float(TIME_FOR_MAX_SPEED_FIELD, 1.0),
                    Element::with_value(crc(ACCEL_FIELD), accel),
                    float(LIFETIME_FIELD, 10.0),
                ],
            )],
        );
        let flight = slice_flight(&slice).expect("flight");
        assert_eq!(
            flight,
            Flight {
                base_speed_mps: 100.0,
                extra_speed_mps: 25.0,
                time_for_max_speed_s: 1.0,
                gravity_mps2: 9.8,
                lifetime_s: 10.0,
            }
        );
        assert_eq!(
            slice_path("Items/Weapons/basic_arrow"),
            "slices/items/weapons/basic_arrow.dynamicslice"
        );
    }
}
