use std::time::{Duration, Instant};

use discordia_bot::{Bot, BotIdentity};

use crate::config::AgentConfig;
use crate::harness::Harness;
use crate::limits::Ledger;
use crate::runner::run;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Online,
    /// Waiting before the next attempt, with why the last one ended.
    Retrying {
        error: String,
        in_secs: u64,
    },
}

const FIRST_WAIT: Duration = Duration::from_secs(2);
const LONGEST_WAIT: Duration = Duration::from_secs(120);
/// A connection that held this long was healthy; the next drop starts over.
const STEADY: Duration = Duration::from_secs(60);

/// Runs until the task is dropped, reconnecting with bounded backoff so a
/// host that is down or refusing is not hammered. One ledger spans every connection.
pub async fn run_forever<H: Harness + Clone>(
    url: String,
    identity: BotIdentity,
    config: AgentConfig,
    harness: H,
    report: impl Fn(Status) + Send + 'static,
) {
    let ledger = Ledger::new(config.limits.clone());
    let mut wait = FIRST_WAIT;
    loop {
        report(Status::Connecting);
        let started = Instant::now();
        let error = match Bot::connect(&url, &identity, &config.name).await {
            Ok(bot) => {
                report(Status::Online);
                match run(
                    bot,
                    &identity,
                    config.clone(),
                    harness.clone(),
                    ledger.clone(),
                )
                .await
                {
                    Ok(()) => "the server closed the connection".to_string(),
                    Err(e) => e.to_string(),
                }
            }
            Err(e) => e.to_string(),
        };
        if started.elapsed() >= STEADY {
            wait = FIRST_WAIT;
        }
        report(Status::Retrying {
            error,
            in_secs: wait.as_secs(),
        });
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(LONGEST_WAIT);
    }
}
