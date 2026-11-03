//! Offline checks of the `norte` facade with no other dependency.
//!
//! All Norte APIs and macros are resolved through the facade. These checks need
//! neither an agent runtime nor network access.

#![forbid(unsafe_code)]

/// Metadata declared with the facade's default macro anchor.
#[norte::tool(name = "echo", description = "Repeat the text back.")]
pub struct Echo;

/// Metadata declared with an explicit facade anchor and an existing `Clone`.
#[norte::tool(name = "echo_explicit", description = "Repeat the text back.", crate = ::norte)]
#[derive(Clone)]
pub struct EchoExplicit;

#[cfg(test)]
mod tests {
    use super::*;
    use norte::ToolMetadata;
    use norte::prelude::*;

    #[test]
    fn core_types_are_available_through_the_facade() {
        let reference = norte::ModelRef::new(
            norte::ProviderId::new("openai").expect("a provider"),
            norte::ModelId::new("gpt-5").expect("a model"),
        );
        assert_eq!(reference.to_string(), "openai/gpt-5");

        let message = Message::user("Hello!");
        assert_eq!(message.role, Role::User);
        assert_eq!(message.parts.len(), 1);
        assert!(matches!(&message.parts[0], Part::Text(text) if text.text == "Hello!"));
        assert!(ToolRegistry::new().is_empty());
        let _: norte::InMemoryBlobStore = InMemoryBlobStore::new();
    }

    #[test]
    fn tool_macros_resolve_both_anchors_through_the_facade() {
        fn metadata<T: ToolMetadata + Clone>(tool: &T) -> (&'static str, &'static str) {
            let _: T = tool.clone();
            (T::NAME, T::DESCRIPTION)
        }

        assert_eq!(metadata(&Echo), ("echo", "Repeat the text back."));
        assert_eq!(
            metadata(&EchoExplicit),
            ("echo_explicit", "Repeat the text back.")
        );
    }

    #[test]
    fn default_provider_builds_a_model_without_network_or_runtime() {
        let capabilities = Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .build();
        let model =
            norte::presets::openai("gpt-5", "sk-test", capabilities.clone()).expect("a model");
        let model: &dyn Model = &model;
        assert_eq!(model.reference().to_string(), "openai/gpt-5");
        assert_eq!(model.capabilities(), &capabilities);
        assert_eq!(norte::openai::WIRE_CHAT, "openai.chat");
    }

    #[test]
    fn tools_feature_exports_the_suite_and_prelude() {
        fn same_type<T>(_: Option<T>, _: Option<T>) {}

        same_type(None::<ShellTool>, None::<norte::tools::ShellTool>);
        same_type(None::<ReadTool>, None::<norte::tools::ReadTool>);
        same_type(None::<WriteTool>, None::<norte::tools::WriteTool>);
        same_type(None::<EditTool>, None::<norte::tools::EditTool>);
    }
}
