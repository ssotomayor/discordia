use std::collections::VecDeque;
use std::sync::Mutex;

use discordia_protocol::push::PushData;

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    /// The token will never work again; its device row goes.
    TokenGone,
    /// Anything else, already retried where retrying helps.
    Failed(String),
}

pub trait Sender: Send + Sync {
    /// `validate_only` asks FCM to check the token and message without delivering.
    fn send<'a>(
        &'a self,
        token: &'a str,
        data: &'a PushData,
        validate_only: bool,
    ) -> BoxFuture<'a, Delivery>;
}

/// The first few characters of an identifier, for a log line that must not
/// carry a token, handle or key whole.
pub fn short(s: &str) -> &str {
    s.get(..8).unwrap_or("…")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub token: String,
    pub data: PushData,
    pub validate_only: bool,
}

/// Records instead of sending: the tests' sender, and the relay's when no FCM
/// credentials are configured, where it also logs each send.
pub struct RecordingSender {
    sent: Mutex<VecDeque<Recorded>>,
    answer: Mutex<Delivery>,
    changed: tokio::sync::Notify,
}

/// Without credentials the relay may run for weeks; the record must not grow with it.
const KEEP: usize = 256;

impl Default for RecordingSender {
    fn default() -> Self {
        Self {
            sent: Mutex::new(VecDeque::new()),
            answer: Mutex::new(Delivery::Sent),
            changed: tokio::sync::Notify::new(),
        }
    }
}

impl RecordingSender {
    /// What every later send reports, e.g. `TokenGone` to play an uninstalled app.
    pub fn answer(&self, delivery: Delivery) {
        *self.answer.lock().unwrap_or_else(|p| p.into_inner()) = delivery;
    }

    pub fn sent(&self) -> Vec<Recorded> {
        self.sent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Delivered sends only, not token checks.
    pub fn delivered(&self) -> Vec<Recorded> {
        self.sent()
            .into_iter()
            .filter(|r| !r.validate_only)
            .collect()
    }

    /// Waits until `n` sends have been recorded or `timeout` passes; `/v1/push`
    /// sends after it answers.
    pub async fn wait_for(&self, n: usize, timeout: std::time::Duration) -> Vec<Recorded> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.changed.notified();
            let sent = self.delivered();
            if sent.len() >= n {
                return sent;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.delivered();
            }
        }
    }
}

impl Sender for RecordingSender {
    fn send<'a>(
        &'a self,
        token: &'a str,
        data: &'a PushData,
        validate_only: bool,
    ) -> BoxFuture<'a, Delivery> {
        Box::pin(async move {
            tracing::info!(
                token = short(token),
                kind = kind(data),
                validate_only,
                "push not sent: no FCM credentials"
            );
            {
                let mut sent = self.sent.lock().unwrap_or_else(|p| p.into_inner());
                if sent.len() == KEEP {
                    sent.pop_front();
                }
                sent.push_back(Recorded {
                    token: token.to_string(),
                    data: data.clone(),
                    validate_only,
                });
            }
            self.changed.notify_waiters();
            self.answer
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        })
    }
}

pub fn kind(data: &PushData) -> &'static str {
    match data {
        PushData::Dm { .. } => "dm",
        PushData::Mention { .. } => "m",
        PushData::Ping => "ping",
        PushData::Unknown => "unknown",
    }
}
