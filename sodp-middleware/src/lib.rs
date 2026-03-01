//! Application-layer middleware for SODP-based servers.
//!
//! This crate has **no dependency on the `sodp` core crate** — it is a
//! standalone toolkit that can back any server or framework built on top of
//! the SODP protocol (or any other transport).
//!
//! # Modules
//!
//! | Module | What it provides |
//! |--------|-----------------|
//! | [`acl`] | Per-key access control — glob rules, `{sub}` substitution |
//! | [`rate`] | Sliding-window per-session rate limiter |
//!
//! # Roadmap
//!
//! Future additions (none yet committed):
//! - **Middleware trait** — composable request pipeline (à la Tower/Starlette)
//! - **Router** — map key patterns to handler functions
//! - **Validation hooks** — pluggable schema validators
//! - **Audit log** — structured access log per key + operation

pub mod acl;
pub mod rate;
