// SPDX-License-Identifier: AGPL-3.0-or-later
//! The interact tags of the level objects a player can interact with (finding "task
//! interact"), by the facet entity an interaction request names. An object's tags come from its
//! slice (`m_interactTagString`, the world file's interact tags) and from its gatherable variant
//! (`VariationData` `InteractTag`, the quest objects `IO_*`). A gather of a region gatherable
//! point, which has no slice, has the tags of its gatherable's variants (the Gawain crown
//! `MB_MSQ2_DefeatGawain_Crown` of `12A_MSQ_24_DefeatGawain`). A `TaskInteract` task counts an
//! interact whose tag matches its `InteractTag`.

use std::collections::HashMap;

use nw_formats::objectstream::lowercase_crc32;

use crate::Tables;
use crate::gathering::{FACET_FREE_BITS, capital_key};

/// The interact tags of the level objects, by object key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InteractTags {
    by_key: HashMap<u64, Vec<String>>,
    /// The tags of the variants of each gatherable, by the CRC-32 of its lowercase
    /// `GatherableEntryID`.
    by_gatherable: HashMap<u32, Vec<String>>,
}

/// The `InteractTag` of each `VariationData` row that has one and a `GatherableEntryID`: the
/// CRC-32 of the lowercase entry id and the tag.
pub fn gatherable_tags(tables: &Tables) -> Vec<(u32, String)> {
    tables
        .variation_data
        .iter()
        .flat_map(|table| &table.rows)
        .filter_map(|row| {
            let tag = row.interact_tag.clone().filter(|tag| !tag.is_empty())?;
            let entry = row
                .gatherable_entry_id
                .as_deref()
                .filter(|id| !id.is_empty())?;
            Some((lowercase_crc32(entry), tag))
        })
        .collect()
}

/// The `InteractTag` of each `VariationData` row that has one, by lowercase `VariantID`.
pub fn variant_tags(tables: &Tables) -> HashMap<String, String> {
    tables
        .variation_data
        .iter()
        .flat_map(|table| &table.rows)
        .filter_map(|row| {
            let tag = row.interact_tag.clone().filter(|tag| !tag.is_empty())?;
            Some((row.variant_id.clone()?.to_lowercase(), tag))
        })
        .collect()
}

impl InteractTags {
    /// The tags of the capitals (GUID text, variant name, slice tags) and the spawned slices
    /// (GDE id, variant name, slice tags), with the tags of their variants in `variants` (by
    /// lowercase variant id). Variant names match in any case.
    pub fn build<'a>(
        variants: &HashMap<String, String>,
        capitals: impl IntoIterator<Item = (&'a str, Option<&'a str>, &'a [String])>,
        spawned: impl IntoIterator<Item = (u64, &'a str, &'a [String])>,
    ) -> Self {
        let tags_of = |variant: Option<&str>, slice: &[String]| -> Vec<String> {
            let mut tags = slice.to_vec();
            if let Some(tag) = variant.and_then(|variant| variants.get(&variant.to_lowercase()))
                && !tags.contains(tag)
            {
                tags.push(tag.clone());
            }
            tags
        };
        let mut by_key = HashMap::new();
        let capitals = capitals.into_iter().filter_map(|(guid, variant, slice)| {
            Some((capital_key(guid)?, tags_of(variant, slice)))
        });
        let spawned = spawned
            .into_iter()
            .map(|(id, variant, slice)| (id & !FACET_FREE_BITS, tags_of(Some(variant), slice)));
        for (key, tags) in capitals.chain(spawned) {
            if !tags.is_empty() {
                by_key.insert(key, tags);
            }
        }
        Self {
            by_key,
            by_gatherable: HashMap::new(),
        }
    }

    /// The same tags, and `gatherables` (the CRC-32 of a lowercase `GatherableEntryID` and a
    /// variant's tag, [`gatherable_tags`]) for the gathers of objects without tags of their own.
    #[must_use]
    pub fn with_gatherables(
        mut self,
        gatherables: impl IntoIterator<Item = (u32, String)>,
    ) -> Self {
        for (gatherable, tag) in gatherables {
            let tags = self.by_gatherable.entry(gatherable).or_default();
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        self
    }

    /// The interact tags of an interaction with the object of facet entity `facet`: its own,
    /// else those of the gatherable it gathers, if any.
    pub fn of_interaction(&self, facet: u64, gatherable: Option<u32>) -> &[String] {
        let own = self.of(facet);
        if !own.is_empty() {
            return own;
        }
        gatherable
            .and_then(|gatherable| self.by_gatherable.get(&gatherable))
            .map_or(&[], Vec::as_slice)
    }

    /// The interact tags of the object with facet entity `facet`; empty for an object with none.
    pub fn of(&self, facet: u64) -> &[String] {
        self.by_key
            .get(&(facet & !FACET_FREE_BITS))
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retail Monarch's Bluffs settlement capital (vault capture 2026-09-27): its storage
    /// interaction names facet entity `0xE0F2_6061_7543_BB93`.
    const SETTLEMENT_GUID: &str = "f7763200-249e-87c4-1784-526151dd3c56";
    const SETTLEMENT_FACET: u64 = 0xE0F2_6061_7543_BB93;
    const JOURNAL_ID: u64 = 0x1234_5678_9abc_def0;

    #[test]
    fn an_object_has_its_slice_tags_and_its_variant_tag() {
        let variants = HashMap::from([(
            "io_99b_msq_02_galahadjournal".to_owned(),
            "IO_99B_MSQ_02_GalahadJournal".to_owned(),
        )]);
        let storage = ["SettlementStorage".to_owned()];
        let tags = InteractTags::build(
            &variants,
            [(SETTLEMENT_GUID, None, storage.as_slice())],
            [(JOURNAL_ID, "IO_99B_MSQ_02_GalahadJournal", &[][..])],
        );
        assert_eq!(tags.of(SETTLEMENT_FACET), ["SettlementStorage"]);
        assert_eq!(tags.of(JOURNAL_ID), ["IO_99B_MSQ_02_GalahadJournal"]);
        assert!(tags.of(1).is_empty());
    }

    #[test]
    fn a_region_gatherable_s_gather_has_its_variant_tag() {
        const CROWN: &str = "MB_MSQ2_DefeatGawain_Crown";
        const CROWN_TAG: &str = "IO_MB_MSQ2_DefeatGawain_Crown";
        const REGION_POINT_FACET: u64 = 0x5555_0000_0000_0001;
        let tags = InteractTags::build(
            &HashMap::new(),
            [(
                SETTLEMENT_GUID,
                None,
                ["SettlementStorage".to_owned()].as_slice(),
            )],
            [],
        )
        .with_gatherables([(lowercase_crc32(CROWN), CROWN_TAG.to_owned())]);
        let crown = Some(lowercase_crc32(CROWN));
        assert_eq!(tags.of_interaction(REGION_POINT_FACET, crown), [CROWN_TAG]);
        assert!(tags.of_interaction(REGION_POINT_FACET, None).is_empty());
        assert_eq!(
            tags.of_interaction(SETTLEMENT_FACET, crown),
            ["SettlementStorage"],
            "an object's own tags come first"
        );
    }
}
