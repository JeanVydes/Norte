//! Browser automation for AI agents.
//!
//! The tool drives a live Chromium browser over the `Chrome DevTools Protocol`.
//! The implementation is split by responsibility: request and output types,
//! host limits, the CDP transport, DOM mapping, action execution and the text
//! renderer.

mod actions;
mod cdp;
mod dom;
mod error;
mod executor;
mod limits;
mod outcome;
mod output;
mod page;
mod render;
mod request;
mod tabs;
mod tool;

pub use limits::{BrowserLimits, BrowserScheme, UrlPolicy};
pub use output::{BrowserOutput, BrowserTab, ContentOrigin, InteractiveElement};
pub use request::{BrowserAction, BrowserKey, BrowserRequest};
pub use tool::{BrowserTool, browser_tool};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
