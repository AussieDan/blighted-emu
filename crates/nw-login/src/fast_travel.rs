// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[fast_travel]` of `config/edge.toml`: whether a fast travel costs azoth. The server writes
//! the setting into the remote config it gives the client
//! (`javelin.playtest-disable-fast-travel-costs`, [`crate::remote_config`]), and the sims read it
//! from there, so the travel cost the map shows and the azoth the server takes match.
//!
//! The default is the retail cost. Observed: retail charges (203 of 266 retail travels to a
//! settlement or a shrine took azoth), and no retail layer sets the key (client default false,
//! cvar registration `FUN_140169720`).

use serde::Deserialize;

/// `[fast_travel]`. A setting left out keeps the retail cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FastTravelConfig {
    /// A fast travel to a settlement or a shrine costs no azoth.
    #[serde(default)]
    pub free: bool,
}
