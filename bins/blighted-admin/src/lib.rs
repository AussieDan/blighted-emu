// SPDX-License-Identifier: AGPL-3.0-or-later
//! `blighted-admin`: admin web tools (admin tools spec): the world map, the capture viewer and
//! the live panel, on a loopback address, over HTTPS, or through the admin relay.

pub mod access;
pub mod assets;
pub mod captures;
pub mod config;
pub mod gm;
pub mod guard;
pub mod host;
pub mod live;
pub mod relay;
pub mod relief;
pub mod run;
pub mod server;
pub mod shapes;
pub mod studio;
pub mod todo;
pub mod world;
