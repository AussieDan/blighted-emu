// SPDX-License-Identifier: AGPL-3.0-or-later
//! Publishing without waiting. One task publishes the queued messages in order, so a slow or lost
//! NATS connection never blocks the caller's loop; a full queue drops the message.

use serde::Serialize;
use tokio::sync::mpsc;

use crate::client::Bus;

/// A queue of messages for NATS, published in order by one task.
#[derive(Debug)]
pub struct Publisher<T> {
    queue: mpsc::Sender<(String, T)>,
}

impl<T> Clone for Publisher<T> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
        }
    }
}

impl<T: Serialize + Send + Sync + 'static> Publisher<T> {
    /// Starts the publishing task with room for `capacity` waiting messages. The task ends when
    /// every clone of the publisher is gone.
    pub fn start(bus: Bus, capacity: usize) -> Self {
        let (queue, mut waiting) = mpsc::channel::<(String, T)>(capacity);
        tokio::spawn(async move {
            while let Some((subject, message)) = waiting.recv().await {
                if let Err(error) = bus.publish(subject, &message).await {
                    tracing::warn!(%error, "bus publish failed");
                }
            }
        });
        Self { queue }
    }

    /// Queues `message` for `subject`. Returns `false` when the queue is full and the message is
    /// dropped.
    pub fn send(&self, subject: String, message: T) -> bool {
        self.try_send(subject, message).is_ok()
    }

    /// Queues `message` for `subject`.
    ///
    /// # Errors
    ///
    /// Returns the message when the queue is full (or the task ended), so the caller may keep it.
    pub fn try_send(&self, subject: String, message: T) -> Result<(), T> {
        self.queue
            .try_send((subject, message))
            .map_err(|error| error.into_inner().1)
    }
}

#[cfg(all(test, feature = "infra"))]
mod tests {
    use std::time::Duration;

    use uuid::Uuid;

    use super::*;
    use crate::id::BusId;
    use crate::message::Heartbeat;
    use crate::test_support::test_bus;

    const MESSAGES: u64 = 500;
    const QUEUE: usize = 1024;
    const WAIT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn messages_arrive_in_the_order_they_were_queued() {
        let bus = test_bus().await;
        let subject = format!("nw.test.{}", Uuid::new_v4());
        let mut received = bus
            .subscribe::<Heartbeat>(subject.clone())
            .await
            .expect("subscribe");
        bus.flush().await.expect("flush");
        let publisher = Publisher::start(bus, QUEUE);
        let id = BusId::try_from("gw-test".to_owned()).expect("id");
        for started_at_ms in 0..MESSAGES {
            let beat = Heartbeat {
                id: id.clone(),
                started_at_ms,
            };
            assert!(publisher.send(subject.clone(), beat));
        }
        for expected in 0..MESSAGES {
            let beat = tokio::time::timeout(WAIT, received.next())
                .await
                .expect("in time")
                .expect("open")
                .expect("decode");
            assert_eq!(beat.message.started_at_ms, expected);
        }
    }
}
