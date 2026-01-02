//! Runners for various programming languages
//!
//! This module provides tools to execute code snippets in different
//! programming languages. Each submodule corresponds to a specific language
//! and contains the necessary functionality to run code in that language.
//!
//! TODO: Doing this module properly with sandboxing and security in mind.

pub mod js;
pub mod lua;
pub mod py;
