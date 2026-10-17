#![forbid(unsafe_code)]
#![doc = "Shared `HTTP` transport for `RustyChain`: `SSE` parsing, vendor profiles, auth schemes, secrets and `Retry-After` handling."]

pub mod auth;
pub mod client;
pub mod media;
pub mod retry;
pub mod secret;
pub mod sse;
pub mod vendor;

pub use auth::AuthScheme;
pub use client::{HttpClient, SseStream, is_retryable};
pub use media::HttpMedia;
pub use retry::{RetryPolicy, parse_retry_after};
pub use secret::Secret;
pub use sse::{SseDecoder, SseEvent};
pub use vendor::VendorProfile;
