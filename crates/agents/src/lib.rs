//! Teams of Claude agents working through Persephone Lab: how a team is drawn, checking a
//! design, turning it into Claude Code sessions, the folder each agent keeps, and running and
//! reading those sessions. It has no interface of its own: the app draws it.

pub mod check;
pub mod control;
pub mod findings;
pub mod folder;
pub mod plan;
pub mod prompts;
pub mod run;
pub mod stream;
pub mod team;
