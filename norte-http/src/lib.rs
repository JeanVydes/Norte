#![forbid(unsafe_code)]
#![doc = "Shared `HTTP` transport for `Norte`: `SSE` parsing, vendor profiles, auth schemes, secrets, and the `TransportPolicy` that bounds its reconnects and timeouts."]

/// Authentication schemes for outgoing requests.
pub mod auth;
/// The HTTP client and the SSE stream it returns.
pub mod client;
/// Downloading media over HTTP.
pub mod media;
/// Vendor deviations from a reference wire format.
pub mod quirk;
/// Secrets that never reach logs or debug output.
pub mod secret;
/// Server-sent events: the event shape and the incremental decoder.
pub mod sse;
pub mod transport;
/// Vendor profiles: where to send requests and how to authenticate.
pub mod vendor;

pub use auth::AuthScheme;
pub use client::{HttpClient, SseStream, is_transient};
pub use media::HttpMedia;
pub use quirk::{Quirk, Quirks};
pub use secret::Secret;
pub use sse::{SseDecoder, SseEvent};
pub use transport::{TransportPolicy, parse_retry_after};
pub use vendor::VendorProfile;
