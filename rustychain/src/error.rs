// src/error.rs

//! Error handling for RustyChain

#[cfg(feature = "google")]
pub use gemini_rust::ClientError;

use std::{any::Any, error::Error as StdError, sync::Arc};
use thiserror::Error;

/// A specialized Result type for the RustyChain core module.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Generic(String),

    // --- LLM PROVIDER ERRORS ---
    #[cfg(feature = "google")]
    #[error("Gemini API Error: {0}")]
    Gemini(#[from] Box<ClientError>),

    #[cfg(feature = "openai")]
    #[error("OpenAI Error: {0}")]
    OpenAI(#[from] Box<async_openai::error::OpenAIError>),

    // --- SERIALIZATION & NETWORK ---
    #[error("Serialization Error: {0}")]
    Serde(#[from] Box<serde_json::Error>),

    #[error("HTTP Error: {0}")]
    HTTP(Box<dyn StdError + Send + Sync>),

    #[error("URL Parse Error: {0}")]
    URL(#[from] Box<url::ParseError>),

    // --- SCRAPING & TOOLS ---
    #[error("HTML Parsing Error: {0}")]
    ParsingHTML(String),

    #[error("Scraper Selector Error: {0}")]
    Scraper(String),

    #[error("Internal Error: {0}")]
    Internal(Box<dyn StdError + Send + Sync>),

    #[error("Not Found: {0}")]
    NotFound(String),

    #[error("No Content")]
    NoContent,

    #[error("Operation timed out")]
    Timeout,

    // --- DATABASE ERRORS ---
    #[cfg(feature = "sql")]
    #[error("Database Error: {0}")]
    Sqlx(#[from] Box<sqlx::Error>),

    #[cfg(feature = "qdrant")]
    #[error("Qdrant Connection Error: {0}")]
    Qdrant(#[from] Box<qdrant_client::QdrantError>),

    #[cfg(feature = "mongodb")]
    #[error("MongoDB Error: {0}")]
    MongoDB(#[from] Box<mongodb::error::Error>),

    #[cfg(feature = "mongodb")]
    #[error("BSON Serialization Error: {0}")]
    Bson(#[from] Box<mongodb::bson::ser::Error>),

    // --- SYSTEM & UTILS ---
    #[error("IO Error: {0}")]
    IO(Box<dyn StdError + Send + Sync>),

    #[error("Invalid Input: {0}")]
    Input(String),

    #[error("Unsupported: {0}")]
    Unsupported(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Downcast Error")]
    Downcast(Arc<dyn Any + Send + Sync>),

    #[error("Tokio Join Error: {0}")]
    TokioJoin(#[from] Box<tokio::task::JoinError>),

    #[error("Tool Error: {source}")]
    ToolError {
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },

    // --- LOGIC ERRORS ---
    #[error("Rate limit exceeded. Retry after {retry_after_secs} seconds")]
    RateLimit { retry_after_secs: u64 },

    #[error("Error in step {index} ('{step_name:?}'): {source}")]
    StepError {
        index: usize,
        step_name: Option<String>,
        source: Box<dyn StdError + Send + Sync>,
    },

    #[error("Initial input not set")]
    NotInput,

    #[error("Chain input validation failed: {message}")]
    Validation { message: String },

    #[error("Tool functions results not returned results")]
    NotFunctionResults,

    #[error("Search Error: {query:?}, Source: {source:?}, Feedback: {feedback:?}")]
    Search {
        query: Option<String>,
        source: Option<Box<dyn StdError + Send + Sync>>,
        feedback: Option<String>,
    },

    #[error("Error in chain step {index} ('{name}'): {source}")]
    ChainError {
        index: usize,
        name: String,
        source: Box<dyn StdError + Send + Sync>,
    },

    #[error("Error in agent at step {index} ('{name}'): {source}")]
    AgentError {
        index: usize,
        name: String,
        source: Box<dyn StdError + Send + Sync>,
    },

    #[error("Non native function call schema extraction error: {0}")]
    Extraction(#[from] ExtractionError),
}

#[derive(Debug, Error)]
pub enum ExtractionError {
    #[error("Field '{0}' not found in JSON object")]
    FieldNotFound(String),

    #[error("Field '{0}' has wrong type: expected {1}, got {2}")]
    WrongType(String, String, String),

    #[error("Failed to deserialize '{field}': {error}")]
    DeserializationError {
        field: String,
        error: serde_json::Error,
    },

    #[error("Invalid function call at index {index}: {error}")]
    InvalidFunctionCall {
        index: usize,
        error: serde_json::Error,
    },

    #[error("JSON value is not an object")]
    NotAnObject,
}

impl Error {
    pub fn from_boxed(err: Box<dyn StdError + Send + Sync + 'static>) -> Self {
        match err.downcast::<Error>() {
            Ok(inner) => *inner,
            Err(err) => Error::Internal(err),
        }
    }
}

// --- CONVERSIONS ---

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::IO(Box::new(err))
    }
}

#[cfg(feature = "sql")]
impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        Error::Sqlx(Box::new(err))
    }
}

#[cfg(feature = "qdrant")]
impl From<qdrant_client::QdrantError> for Error {
    fn from(err: qdrant_client::QdrantError) -> Self {
        Error::Qdrant(Box::new(err))
    }
}

#[cfg(feature = "mongodb")]
impl From<mongodb::error::Error> for Error {
    fn from(err: mongodb::error::Error) -> Self {
        Error::MongoDB(Box::new(err))
    }
}

#[cfg(feature = "mongodb")]
impl From<mongodb::bson::ser::Error> for Error {
    fn from(err: mongodb::bson::ser::Error) -> Self {
        Error::Bson(Box::new(err))
    }
}

#[cfg(feature = "openai")]
impl From<async_openai::error::OpenAIError> for Error {
    fn from(err: async_openai::error::OpenAIError) -> Self {
        Error::OpenAI(Box::new(err))
    }
}

#[cfg(feature = "google")]
impl From<ClientError> for Error {
    fn from(err: ClientError) -> Self {
        Error::Gemini(Box::new(err))
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Serde(Box::new(err))
    }
}

impl From<url::ParseError> for Error {
    fn from(err: url::ParseError) -> Self {
        Error::URL(Box::new(err))
    }
}

impl From<tokio::task::JoinError> for Error {
    fn from(err: tokio::task::JoinError) -> Self {
        Error::TokioJoin(Box::new(err))
    }
}

impl From<Box<dyn StdError + Send + Sync>> for Error {
    fn from(err: Box<dyn StdError + Send + Sync>) -> Self {
        Error::Internal(err)
    }
}

impl From<base64::DecodeError> for Error {
    fn from(err: base64::DecodeError) -> Self {
        Error::Internal(Box::new(err))
    }
}

impl From<Box<dyn StdError>> for Error {
    fn from(err: Box<dyn StdError>) -> Self {
        Error::Internal(Box::from(err.to_string()))
    }
}

impl From<Arc<dyn std::any::Any + std::marker::Send + Sync>> for Error {
    fn from(err: Arc<dyn std::any::Any + std::marker::Send + Sync>) -> Self {
        Error::Downcast(err)
    }
}

impl From<String> for Error {
    fn from(err: String) -> Self {
        Error::Generic(err)
    }
}