// SPDX-License-Identifier: AGPL-3.0-or-later
//! `nw-testclient` (M3 spec section 8): the client side of the REP connection, used by the test
//! client program and by the `rep-gateway` tests.

pub mod activity;
pub mod chat;
pub mod config;
pub mod fleet;
pub mod gather;
pub mod hunt;
pub mod pair;
pub mod play;
pub mod rep;
pub mod route;
pub mod swarm;
pub mod sync;
pub mod udp;
pub mod view;
pub mod walk;
