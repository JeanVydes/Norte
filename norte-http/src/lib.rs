#![forbid(unsafe_code)]
#![doc = "Shared `HTTP` transport for `Norte`: `SSE` parsing, vendor profiles, auth schemes, secrets, and the `TransportPolicy` that bounds its reconnects and timeouts."]

pub mod auth;
pub mod client;
pub mod media;
pub mod quirk;
pub mod secret;
pub mod sse;
pub mod transport;
pub mod vendor;

pub use auth::AuthScheme;
pub use client::{HttpClient, SseStream, is_transient};
pub use media::HttpMedia;
pub use quirk::{Quirk, Quirks};
pub use secret::Secret;
pub use sse::{SseDecoder, SseEvent};
pub use transport::{TransportPolicy, parse_retry_after};
pub use vendor::VendorProfile;
