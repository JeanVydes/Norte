use secrecy::SecretString;

use crate::{
    llm::{AnyFunction, LLM},
    providers::LLMProvider,
};
use std::sync::Arc;

/// Builder for constructing LLM instances with customizable parameters.
#[derive(Default, Debug, Clone)]
pub struct LLMBuilder {
    /// Model name or identifier.
    pub model: Option<String>,
    /// System prompt to guide the LLM's behavior.
    pub system_prompt: Option<String>,
    /// LLM provider (e.g., OpenAI, Google).
    pub provider: Option<LLMProvider>,
    /// Authorization token or credentials.
    pub authorization: Option<SecretString>,
    /// Collection of tools (functions) to enhance LLM capabilities.
    pub tools: Vec<Arc<dyn AnyFunction>>,
    /// Optional custom endpoint for the LLM API.
    pub endpoint: Option<String>,
}

impl LLMBuilder {
    /// Builds the LLM instance from the configured parameters.
    pub fn build(self) -> crate::Result<LLM> {
        let model = match self.model {
            Some(n) => n,
            None => {
                return Err(crate::Error::Input("LLM name not set".to_owned()));
            }
        };

        let provider = match self.provider {
            Some(p) => p,
            None => {
                return Err(crate::Error::Input("LLM provider not set".to_owned()));
            }
        };

        let mut names = Vec::new();
        for tool in &self.tools {
            if names.contains(&tool.name().to_owned()) {
                return Err(crate::Error::Input(format!(
                    "Duplicate tool name detected: {}",
                    tool.name()
                )));
            }

            names.push(tool.name().to_owned());
        }

        Ok(LLM {
            model,
            system_prompt: self.system_prompt.unwrap_or_default(),
            provider,
            authorization: self.authorization,
            tools: self.tools,
            endpoint: self.endpoint,
        })
    }

    /// Adds a single tool to the LLM builder.
    pub fn add_tool(mut self, tool: impl Into<Arc<dyn AnyFunction>>) -> Self {
        self.tools.push(tool.into());
        self
    }

    /// Adds multiple tools to the LLM builder.
    pub fn add_tools(mut self, tools: Vec<impl Into<Arc<dyn AnyFunction>>>) -> Self {
        for tool in tools {
            self.tools.push(tool.into());
        }
        self
    }

    /// Sets the authorization token.
    pub fn set_authorization(mut self, authorization: impl ToString) -> Self {
        self.authorization = Some(SecretString::new(authorization.to_string()));
        self
    }

    /// Sets the model name.
    pub fn set_model(mut self, name: impl ToString) -> Self {
        self.model = Some(name.to_string());
        self
    }

    /// Sets the system prompt.
    pub fn set_system_prompt(mut self, prompt: impl ToString) -> Self {
        self.system_prompt = Some(prompt.to_string());
        self
    }

    /// Sets the LLM provider.
    pub fn set_provider(mut self, provider: LLMProvider) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Sets a custom endpoint for the LLM API.
    pub fn set_endpoint(mut self, endpoint: impl ToString) -> Self {
        self.endpoint = Some(endpoint.to_string());
        self
    }
}
