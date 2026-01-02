use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Image {
    pub url: String,
    pub text: Option<String>,
}

impl Image {
    pub fn new(url: String, text: Option<String>) -> Self {
        Self { url, text }
    }
}
