// SPDX-License-Identifier: AGPL-3.0-or-later
//! `edge-http` (M2 spec section 3.2): reads the configuration and secrets, connects the stores,
//! and serves the `nw-login` routes over HTTPS. Metrics are served on an internal address. The
//! `[aws_mirror]` hosts get this server's own certificate.

mod database;
mod layers;
mod run;
mod tls;

pub use run::{RunError, link_discord, make_mirror_certificate, migrate, serve};
