//! # Norte
//!
//! The Norte stack behind one dependency. The crate root is `norte-core`, and
//! every other crate is a module named after it: `norte::openai`,
//! `norte::anthropic`, `norte::gemini`, `norte::http`, `norte::tools`,
//! `norte::media` and `norte::mcp`.
//!
//! ```
//! # use norte::{Capabilities, Modality};
//! let capabilities = Capabilities::builder()
//!     .input(Modality::Text)
//!     .output(Modality::Text)
//!     .build();
//! let model = norte::presets::openai("gpt-5", "sk-test", capabilities)?;
//! # Ok::<(), norte::Error>(())
//! ```
//!
//! # Features
//!
//! | Feature | Adds |
//! | --- | --- |
//! | `openai` (default) | `norte::openai`, and `http` with it |
//! | `anthropic` | `norte::anthropic` |
//! | `gemini` (alias `google`) | `norte::gemini` |
//! | `tools` | `norte::tools` |
//! | `media` | `norte::media` |
//! | `mcp` | `norte::mcp` |
//! | `testing` | `norte::testing`, for a test target |
//! | `full` | every provider, `tools`, `media` and `mcp` |

#![forbid(unsafe_code)]

pub use norte_core::*;

/// The `#[tool]` attribute, with `ToolMetadata` resolved through this crate.
pub use norte_macros::tool_via_facade as tool;

#[cfg(feature = "http")]
pub use norte_http as http;

#[cfg(feature = "anthropic")]
pub use norte_anthropic as anthropic;
#[cfg(feature = "gemini")]
pub use norte_gemini as gemini;
#[cfg(feature = "mcp")]
pub use norte_mcp as mcp;
#[cfg(feature = "media")]
pub use norte_media as media;
#[cfg(feature = "openai")]
pub use norte_openai as openai;
#[cfg(feature = "testing")]
pub use norte_testing as testing;
#[cfg(feature = "tools")]
pub use norte_tools as tools;

#[cfg(feature = "anthropic")]
pub use norte_anthropic::AnthropicMessages;
#[cfg(feature = "gemini")]
pub use norte_gemini::GeminiGenerate;
#[cfg(feature = "openai")]
pub use norte_openai::{OpenAiChat, OpenAiResponses};

/// The vendor presets, side by side: one function per provider, each returning a
/// ready adapter for a model id, an API key and a [`Capabilities`] declaration.
///
/// ```
/// # use norte::{Capabilities, Modality};
/// # let capabilities = Capabilities::builder().input(Modality::Text).build();
/// let model = norte::presets::openai("gpt-5", "sk-test", capabilities)?;
/// # Ok::<(), norte::Error>(())
/// ```
pub mod presets {
    #[cfg(feature = "anthropic")]
    pub use norte_anthropic::presets::{anthropic, anthropic_profile};
    #[cfg(feature = "gemini")]
    pub use norte_gemini::presets::{gemini, gemini_profile};
    #[cfg(feature = "openai")]
    pub use norte_openai::presets::{openai, openai_profile};
}

/// The types an application imports once.
///
/// `Result` stays out: `norte_core::Result<T>` takes one type parameter and would
/// shadow the standard `Result` of `fn main() -> Result<(), Box<dyn Error>>`.
/// Import `norte::Result` where that alias is what you want, as a tool
/// implementation does.
pub mod prelude {
    pub use norte_core::{
        Capabilities, Decision, Engine, EngineBuilder, EngineEvent, Error, FnExecutor,
        InMemoryBlobStore, InMemoryStore, Layer, Message, Modality, Model, ModelRef, Observer,
        Part, Policy, PolicySet, PolicyView, Role, StreamDelta, ToolOutput, ToolRegistry,
        TurnOptions, Usage,
    };

    #[cfg(feature = "tools")]
    pub use norte_tools::{CommandTool, EditTool, ReadTool, WriteTool};
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[cfg(feature = "openai")]
    #[test]
    fn a_turn_can_be_wired_through_the_facade_alone() {
        let capabilities = Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .build();
        let model = presets::openai("gpt-5", "sk-test", capabilities).expect("a model");

        let engine = EngineBuilder::new()
            .store(Arc::new(InMemoryStore::new()))
            .blobs(Arc::new(InMemoryBlobStore::new()))
            .model(Arc::new(model))
            .tools(ToolRegistry::new())
            .build()
            .expect("an engine");

        assert_eq!(engine.tools().len(), 0);
        assert_eq!(openai::WIRE_CHAT, "openai.chat");
    }

    #[test]
    fn the_root_is_the_core() {
        let reference = ModelRef::new(
            ProviderId::new("openai").expect("a provider"),
            ModelId::new("gpt-5").expect("a model"),
        );
        assert_eq!(reference.to_string(), "openai/gpt-5");
    }
}
