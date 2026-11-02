//! A person's own LLM agents (#453), run on this device. The engine is
//! `discordia-agent`; this holds where they are kept and what keeps them running.

pub mod dms;
pub mod service;
pub mod store;

pub use service::{Agents, RunState, current_server, use_agents};
pub use store::{AgentEntry, Preset, Secrets, ServerRef};
