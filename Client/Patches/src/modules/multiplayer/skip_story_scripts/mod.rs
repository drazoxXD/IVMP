//! Compatibility module for the story-startup patch location.
//!
//! The implementation belongs to `boot::startup_scripts_skip`; this module
//! remains as a domain-level re-export so callers can find it under the
//! multiplayer patch grouping too.

pub use crate::modules::boot::startup_scripts_skip::{restore_story_only, STARTUP_PATTERN};
