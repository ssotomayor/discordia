//! An agent is an app whose replies come from an LLM (#451). This crate is
//! the runner every place that hosts one shares; it holds no key it was not handed.

mod config;
mod context;
mod harness;
mod limits;
mod runner;
mod split;
mod supervise;

pub use config::{AgentConfig, ApiKey, Limits, ModelConfig, Provider};
pub use context::system_prompt;
pub use harness::{Harness, HarnessError, Http, Prompt, Reply, Role, Turn};
pub use limits::{Ledger, Refusal};
pub use runner::run;
pub use supervise::{Status, run_forever};
