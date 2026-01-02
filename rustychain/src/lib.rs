//! RustyChain: A modular framework for building LLM-powered applications in Rust.
//! This crate provides core functionalities, including LLM integration, tool management,
//! document splitting, and vector storage.
extern crate self as rustychain;

#[cfg(any(feature = "openai", feature = "google"))]
pub mod agent;
pub mod chain;
pub mod error;
#[cfg(any(feature = "openai", feature = "google"))]
#[allow(ambiguous_glob_reexports)]
pub mod execution;
#[cfg(any(feature = "openai", feature = "google"))]
pub mod llm;
pub mod orchestrator;
pub mod providers;
pub mod splitters;
pub mod storage;
pub mod util;
pub mod context;

pub use error::*;
#[cfg(any(feature = "openai", feature = "google"))]
#[allow(ambiguous_glob_reexports)]
pub use llm::*;
pub use splitters::*;
pub use storage::*;

pub const BASE64_ENGINE: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub mod prelude {
    //! Prelude
    //!
    //! This module re-exports commonly used types and traits.

    pub use crate::error::Error;
    #[cfg(any(feature = "openai", feature = "google"))]
    pub use crate::function::{
        AnyFunction, FnDeclarator, FnExecutor, FunctionDeclaration, FunctionResult,
    };
    #[cfg(any(feature = "openai", feature = "google"))]
    pub use crate::inference::{Inference, InferenceContent};
    #[cfg(any(feature = "openai", feature = "google"))]
    pub use crate::llm::{GenerationConfig, LLM, LLMEmbedding, LLMGeneration, LLMStreaming, Role};
    pub use crate::providers::LLMProvider;
    pub use crate::util::formatters::Cleaner;
    pub use crate::util::formatters::Formatter;
    #[cfg(feature = "macros")]
    pub use rustychain_macros::*;
    pub use schemars::JsonSchema;
}
