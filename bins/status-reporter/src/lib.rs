// SPDX-License-Identifier: AGPL-3.0-or-later
//! `status-reporter`: one per world, on the game machine. Every 15 s it reads the metrics pages
//! of the world's servers and sends the world status to the public website; every 5 minutes it
//! reads the leaderboards from the databases and sends them. Data only goes out: the website
//! opens no connection to the game machine and never reads its databases. Each report is signed
//! with the world's secret (HMAC-SHA256), which the website checks.

pub mod boards;
pub mod config;
pub mod run;
pub mod signing;
pub mod status;
