// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sim` (M4 spec section 7): one cell of one level at 30 Hz. Players join through the bus, get
//! the login messages, see the players near them in replication bundles, and leave.

pub mod asset_solids;
pub mod camera;
pub mod charting;
pub mod combat;
pub mod config;
pub mod forward;
pub mod gameplay;
pub mod gateways;
pub mod instance_link;
pub mod invite_rules;
pub mod login;
pub mod mob_ai;
pub mod mobs;
pub mod objectives;
pub mod placed_points;
pub mod player_content;
pub mod quest_walk;
pub mod run;
pub mod saves;
pub mod server;
pub mod shop;
pub mod social;
pub mod stats;
pub mod trading_post;
pub mod travel_rules;
pub mod world;
