//! Graders: functions from a run to scores. Deterministic graders run first
//! (fast, free, inside the still-live workspace); LLM judges run after all
//! agent runs finish; human review happens in the desktop app.

pub mod deterministic;
pub mod judge;
