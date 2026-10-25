/// Vendor state that must be replayed unchanged.
pub mod continuity;
/// Token-level probabilities, when a turn asks for them.
pub mod logprob;
/// Media parts and where their bytes live.
pub mod media;
/// Messages, their parts and the usage they report.
pub mod message;
/// Safety ratings a provider attaches to a completion.
pub mod moderation;
/// The parts a message is made of.
pub mod part;
/// Who authored a message.
pub mod role;
