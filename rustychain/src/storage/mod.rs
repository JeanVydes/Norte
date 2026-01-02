//! Storage
//!
//! This module provides storage-related pluggable backends.

pub mod memory;
#[cfg(feature = "persistent_storage")]
pub mod persistent;

pub mod vector_store;
pub use vector_store::*;
