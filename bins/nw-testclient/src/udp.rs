// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runs a [`RepClient`] over a UDP socket: registration, then a hold with ping echo, then close.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::net::UdpSocket;

use crate::rep::{Received, RepClient, RepError, RepEvent};

/// Time between polls of the client.
pub const TICK: Duration = Duration::from_millis(10);
/// Largest UDP datagram read.
pub const MAX_UDP_DATAGRAM: usize = 64 * 1024;
/// Server pings per second of hold: about one (spec section 15 item 4).
pub const EXPECTED_PINGS_PER_SECOND: f64 = 1.0;
/// Share of the expected pings that must arrive, so a delay or a loss does not fail the run.
pub const MIN_PING_SHARE: f64 = 0.5;
/// Any local address and port.
pub const ANY_LOCAL: &str = "0.0.0.0:0";

/// How long a client stays after its registration response.
#[derive(Debug, Clone)]
pub enum Hold {
    /// This long after the response.
    For(Duration),
    /// Until the value turns `true` (also before the response).
    UntilStopped(tokio::sync::watch::Receiver<bool>),
}

impl Hold {
    /// Whether the hold is over, `held` after the response (`None` before it).
    pub fn is_over(&self, held: Option<Duration>) -> bool {
        match self {
            Self::For(hold) => held.is_some_and(|held| held >= *hold),
            Self::UntilStopped(stop) => *stop.borrow(),
        }
    }
}

/// What happened on the connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepOutcome {
    /// Registration status, when a response arrived.
    pub status: Option<u32>,
    /// Server version text of the response.
    pub server_version: Option<String>,
    /// Pings echoed during the hold.
    pub pings: u32,
    /// Time from the registration response to the end of the run.
    pub held: Duration,
    /// Whether the server closed the connection.
    pub closed_by_server: bool,
    /// The other server messages, in order.
    pub messages: Vec<Received>,
}

impl RepOutcome {
    /// Whether at least [`MIN_PING_SHARE`] of the expected pings arrived during the hold, and at
    /// least one.
    pub fn pings_kept_up(&self) -> bool {
        let expected = self.held.as_secs_f64() * EXPECTED_PINGS_PER_SECOND;
        self.pings > 0 && f64::from(self.pings) >= expected * MIN_PING_SHARE
    }
}

/// A failed REP run.
#[derive(Debug, Error)]
pub enum RunError {
    /// The UDP socket failed.
    #[error("UDP socket failed: {0}")]
    Socket(#[from] std::io::Error),
    /// The protocol failed.
    #[error(transparent)]
    Rep(#[from] RepError),
}

/// Runs `client` against `gateway` until the server closes, `timeout` passes without a
/// registration response, or the `hold` is over. Then it closes the connection.
///
/// # Errors
///
/// Returns [`RunError`] when the socket or a protocol layer fails.
pub async fn run_rep(
    gateway: SocketAddr,
    client: RepClient,
    hold: Hold,
    timeout: Duration,
) -> Result<RepOutcome, RunError> {
    run_rep_with(gateway, client, hold, timeout, |_, _, _| Ok(())).await
}

/// [`run_rep`], and after the registration response `on_tick` runs about every [`TICK`] with the
/// client, the outcome so far, and the time since the response; it may queue client messages and
/// take the messages it has read out of the outcome.
///
/// # Errors
///
/// Returns [`RunError`] when the socket, a protocol layer, or `on_tick` fails.
pub async fn run_rep_with(
    gateway: SocketAddr,
    client: RepClient,
    hold: Hold,
    timeout: Duration,
    on_tick: impl FnMut(&mut RepClient, &mut RepOutcome, Duration) -> Result<(), RepError>,
) -> Result<RepOutcome, RunError> {
    let socket = UdpSocket::bind(ANY_LOCAL).await?;
    run_rep_on(socket, gateway, client, hold, timeout, on_tick).await
}

/// [`run_rep_with`] on `socket`.
///
/// # Errors
///
/// Returns [`RunError`] when the socket, a protocol layer, or `on_tick` fails.
pub async fn run_rep_on(
    socket: UdpSocket,
    gateway: SocketAddr,
    mut client: RepClient,
    hold: Hold,
    timeout: Duration,
    mut on_tick: impl FnMut(&mut RepClient, &mut RepOutcome, Duration) -> Result<(), RepError>,
) -> Result<RepOutcome, RunError> {
    let started = Instant::now();
    let now = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut outcome = RepOutcome::default();
    let mut registered_at: Option<Instant> = None;
    let mut buffer = vec![0_u8; MAX_UDP_DATAGRAM];
    loop {
        let held = hold.is_over(registered_at.map(|at| at.elapsed()));
        let waited = registered_at.is_none() && started.elapsed() >= timeout;
        // A client that closed itself (`on_tick`, its work done) ends the run.
        if outcome.closed_by_server || held || waited || client.is_closed() {
            break;
        }
        if let Some(at) = registered_at {
            on_tick(&mut client, &mut outcome, at.elapsed())?;
        }
        for datagram in client.poll(now())? {
            socket.send_to(&datagram, gateway).await?;
        }
        let Ok(received) = tokio::time::timeout(TICK, socket.recv_from(&mut buffer)).await else {
            continue;
        };
        let (len, _) = received?;
        for event in client.receive(now(), &buffer[..len])? {
            match event {
                RepEvent::Connected => {}
                RepEvent::Registered(response) => {
                    outcome.status = Some(response.uint32_at_0x8);
                    outcome.server_version =
                        Some(String::from_utf8_lossy(&response.bytes_at_0x38).into_owned());
                    registered_at = Some(Instant::now());
                }
                RepEvent::Pinged => outcome.pings += 1,
                RepEvent::Closed { .. } => outcome.closed_by_server = true,
                RepEvent::Message(received) => outcome.messages.push(received),
            }
        }
    }
    outcome.held = registered_at.map_or(Duration::ZERO, |at| at.elapsed());
    if !outcome.closed_by_server {
        client.close()?;
        for datagram in client.poll(now())? {
            socket.send_to(&datagram, gateway).await?;
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: Duration = Duration::from_secs(60);
    const FULL_PINGS: u32 = 60;
    const EARLY_STOP_PINGS: u32 = 3;

    #[test]
    fn a_held_client_stops_when_told() {
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let hold = Hold::UntilStopped(stopped);
        assert!(!hold.is_over(Some(HOLD * 10)), "no time limit");
        assert!(!hold.is_over(None));
        stop.send(true).expect("send");
        assert!(hold.is_over(Some(Duration::ZERO)));
        assert!(hold.is_over(None), "also before the registration");
    }

    #[test]
    fn a_timed_hold_is_unchanged() {
        let hold = Hold::For(HOLD);
        assert!(!hold.is_over(None), "the registration timeout decides then");
        assert!(!hold.is_over(Some(HOLD - Duration::from_millis(1))));
        assert!(hold.is_over(Some(HOLD)));
    }

    #[test]
    fn pings_must_last_the_whole_hold() {
        let outcome = |pings| RepOutcome {
            pings,
            held: HOLD,
            ..RepOutcome::default()
        };
        assert!(outcome(FULL_PINGS).pings_kept_up());
        assert!(!outcome(EARLY_STOP_PINGS).pings_kept_up());
        assert!(!outcome(0).pings_kept_up());
    }
}
