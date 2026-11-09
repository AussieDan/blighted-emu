// SPDX-License-Identifier: AGPL-3.0-or-later
//! The status report (`POST /api/ingest/status`): the state of each part of the world, the
//! players online and the round trip of the slowest players, from the servers' metrics pages.
//!
//! A part is up when every metrics page of it answers with a Prometheus text page, also an empty
//! one: a server writes a counter only after its first event, so a server that just started has
//! an empty page. A page that does not answer in time, answers an error or answers another kind
//! of page (such as HTML from a wrong address) makes its part down. The website
//! shows the world Offline when the gateway or the sims are down and Having problems when another
//! part is down.

use futures_util::future::join_all;
use nw_common::prometheus::Scrape;
use reqwest::Client;
use reqwest::header::CONTENT_TYPE;
use serde::Serialize;

use crate::config::PartPages;

/// The players of one sim (`sim::server::METRIC_PLAYERS`).
pub const SIM_PLAYERS_METRIC: &str = "sim_players";
/// The 99th percentile round trip of the players of one gateway worker, 0 for a worker without
/// players (`rep_gateway::server::METRIC_ROUND_TRIP_P99_MS`).
pub const GATEWAY_ROUND_TRIP_P99_METRIC: &str = "rep_gateway_round_trip_p99_ms";
/// The content type of a Prometheus text page (`text/plain`, maybe with a version or charset).
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain";

/// One part of the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// `edge-http`: the launcher login and the game routes.
    Login,
    /// `rep-gateway`: the players' game connections.
    Gateway,
    /// `world-director`.
    Director,
    /// Every `sim` of the world.
    Sims,
    /// `social`.
    Social,
}

impl Part {
    /// Every part.
    pub const ALL: [Part; 5] = [
        Part::Login,
        Part::Gateway,
        Part::Director,
        Part::Sims,
        Part::Social,
    ];

    /// The metrics pages of this part in `pages`.
    pub fn urls(self, pages: &PartPages) -> &[String] {
        match self {
            Part::Login => &pages.login,
            Part::Gateway => &pages.gateway,
            Part::Director => &pages.director,
            Part::Sims => &pages.sims,
            Part::Social => &pages.social,
        }
    }
}

/// Whether a part works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PartState {
    /// Every page of it answered.
    Up,
    /// A page of it did not answer.
    Down,
}

/// The state of each part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Parts {
    /// [`Part::Login`].
    pub login: PartState,
    /// [`Part::Gateway`].
    pub gateway: PartState,
    /// [`Part::Director`].
    pub director: PartState,
    /// [`Part::Sims`].
    pub sims: PartState,
    /// [`Part::Social`].
    pub social: PartState,
}

/// One status report, as the website reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReport {
    /// The state of each part.
    pub parts: Parts,
    /// The players in the sims that answered.
    pub players_online: u32,
    /// The largest 99th percentile round trip of the gateway workers, in milliseconds; 0 without
    /// players or without the gateway.
    pub rtt_p99_ms: u32,
}

/// The pages of one part: each page's samples, `None` for a page that did not answer.
#[derive(Debug, Clone, PartialEq)]
pub struct PartScrapes {
    /// The part.
    pub part: Part,
    /// Its pages, in config order.
    pub pages: Vec<Option<Scrape>>,
}

impl PartScrapes {
    fn state(&self) -> PartState {
        if !self.pages.is_empty() && self.pages.iter().all(Option::is_some) {
            PartState::Up
        } else {
            PartState::Down
        }
    }

    fn answered(&self) -> impl Iterator<Item = &Scrape> {
        self.pages.iter().flatten()
    }
}

/// A metric value as a whole number of at least 0 (a float cast saturates; `NaN` gives 0).
fn whole(value: f64) -> u32 {
    value.round() as u32
}

/// The report of `scrapes`; a part without scrapes is down.
pub fn status_report(scrapes: &[PartScrapes]) -> StatusReport {
    let of = |part: Part| scrapes.iter().find(|scrapes| scrapes.part == part);
    let state = |part: Part| of(part).map_or(PartState::Down, PartScrapes::state);
    let players: f64 = of(Part::Sims)
        .map(|sims| {
            sims.answered()
                .map(|page| page.total(SIM_PLAYERS_METRIC, None))
                .sum()
        })
        .unwrap_or_default();
    let round_trip = of(Part::Gateway)
        .and_then(|gateway| {
            gateway
                .answered()
                .filter_map(|page| page.max(GATEWAY_ROUND_TRIP_P99_METRIC))
                .max_by(f64::total_cmp)
        })
        .unwrap_or_default();
    StatusReport {
        parts: Parts {
            login: state(Part::Login),
            gateway: state(Part::Gateway),
            director: state(Part::Director),
            sims: state(Part::Sims),
            social: state(Part::Social),
        },
        players_online: whole(players),
        rtt_p99_ms: whole(round_trip),
    }
}

/// The samples of the page `url`; `None` when it does not answer with a Prometheus text page.
async fn fetch_page(http: &Client, url: &str) -> Option<Scrape> {
    let result = async {
        let response = http.get(url).send().await?.error_for_status()?;
        let prometheus = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with(PROMETHEUS_CONTENT_TYPE));
        Ok::<_, reqwest::Error>((prometheus, response.text().await?))
    }
    .await;
    match result {
        Ok((true, text)) => Some(Scrape::parse(&text)),
        Ok((false, _)) => {
            tracing::debug!(%url, "metrics page is no Prometheus text page");
            None
        }
        Err(error) => {
            tracing::debug!(%url, %error, "metrics page not read");
            None
        }
    }
}

/// Reads every page of `pages` at the same time (each within the client's time limit).
pub async fn scrape_parts(http: &Client, pages: &PartPages) -> Vec<PartScrapes> {
    join_all(Part::ALL.map(|part| async move {
        PartScrapes {
            part,
            pages: join_all(part.urls(pages).iter().map(|url| fetch_page(http, url))).await,
        }
    }))
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const SIM_PAGE: &str = "sim_players 3\n";
    const OTHER_SIM_PAGE: &str = "sim_players 4\n";
    const GATEWAY_PAGE: &str = "rep_gateway_round_trip_p99_ms{worker=\"0\"} 0\n\
         rep_gateway_round_trip_p99_ms{worker=\"1\"} 87.4\n\
         rep_gateway_round_trip_p99_ms{worker=\"2\"} 41\n";
    const OTHER_PAGE: &str = "director_players 7\n";
    /// The page of a server that has counted nothing yet.
    const EMPTY_PAGE: &str = "";
    const TIMEOUT: Duration = Duration::from_secs(2);

    fn up(part: Part, texts: &[&str]) -> PartScrapes {
        PartScrapes {
            part,
            pages: texts.iter().map(|text| Some(Scrape::parse(text))).collect(),
        }
    }

    fn all_up() -> Vec<PartScrapes> {
        vec![
            up(Part::Login, &[OTHER_PAGE]),
            up(Part::Gateway, &[GATEWAY_PAGE]),
            up(Part::Director, &[OTHER_PAGE]),
            up(Part::Sims, &[SIM_PAGE, OTHER_SIM_PAGE]),
            up(Part::Social, &[OTHER_PAGE]),
        ]
    }

    #[test]
    fn the_metric_names_are_those_the_servers_write() {
        assert_eq!(SIM_PLAYERS_METRIC, sim::server::METRIC_PLAYERS);
        assert_eq!(
            GATEWAY_ROUND_TRIP_P99_METRIC,
            rep_gateway::server::METRIC_ROUND_TRIP_P99_MS
        );
    }

    #[test]
    fn every_part_up_counts_the_players_and_the_slowest_worker() {
        let report = status_report(&all_up());
        assert_eq!(
            report.parts,
            Parts {
                login: PartState::Up,
                gateway: PartState::Up,
                director: PartState::Up,
                sims: PartState::Up,
                social: PartState::Up,
            }
        );
        assert_eq!((report.players_online, report.rtt_p99_ms), (7, 87));
    }

    #[test]
    fn one_silent_sim_makes_the_sims_down_and_keeps_the_others_players() {
        let mut scrapes = all_up();
        scrapes[3].pages[1] = None;
        let report = status_report(&scrapes);
        assert_eq!(report.parts.sims, PartState::Down);
        assert_eq!(report.players_online, 3);
    }

    #[test]
    fn a_part_without_pages_or_scrapes_is_down() {
        let mut scrapes = all_up();
        scrapes[1].pages.clear();
        scrapes.retain(|scrapes| scrapes.part != Part::Social);
        let report = status_report(&scrapes);
        assert_eq!(
            (report.parts.gateway, report.parts.social, report.rtt_p99_ms),
            (PartState::Down, PartState::Down, 0)
        );
    }

    #[test]
    fn the_report_has_the_website_json_shape() {
        let json = serde_json::to_value(status_report(&all_up())).expect("json");
        assert_eq!(
            json,
            serde_json::json!({
                "parts": {
                    "login": "up",
                    "gateway": "up",
                    "director": "up",
                    "sims": "up",
                    "social": "up"
                },
                "playersOnline": 7,
                "rttP99Ms": 87
            })
        );
    }

    #[tokio::test]
    async fn text_pages_are_up_even_empty_and_others_are_down() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let app = axum::Router::new()
            .route("/metrics", axum::routing::get(|| async { SIM_PAGE }))
            .route("/empty", axum::routing::get(|| async { EMPTY_PAGE }))
            .route(
                "/html",
                axum::routing::get(|| async { axum::response::Html("<html>login</html>") }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await });
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let closed_url = format!("http://{}/metrics", closed.local_addr().expect("address"));
        drop(closed);
        let good = format!("http://{address}/metrics");
        let pages = PartPages {
            login: vec![format!("http://{address}/html")],
            gateway: vec![closed_url],
            director: vec![format!("http://{address}/empty")],
            sims: vec![good.clone(), good.clone()],
            social: Vec::new(),
        };
        let http = crate::run::http_client(TIMEOUT).expect("client");
        let report = status_report(&scrape_parts(&http, &pages).await);
        assert_eq!(
            report.parts,
            Parts {
                login: PartState::Down,
                gateway: PartState::Down,
                director: PartState::Up,
                sims: PartState::Up,
                social: PartState::Down,
            }
        );
        assert_eq!(report.players_online, 6);
    }
}
