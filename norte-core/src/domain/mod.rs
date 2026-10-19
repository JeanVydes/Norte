/// One atomic change to a session.
pub mod commit;
/// Vendor state that must be replayed unchanged.
pub mod continuity;
/// The portable document used by export and import.
pub mod conversation;
/// Token-level probabilities, when a turn asks for them.
pub mod logprob;
/// Media parts and where their bytes live.
pub mod media;
/// Messages, their parts and the usage they report.
pub mod message;
/// Safety ratings a provider attaches to a completion.
pub mod moderation;
/// The durable nodes a session's branch is made of.
pub mod node;
/// The parts a message is made of.
pub mod part;
/// Who authored a message.
pub mod role;
/// Durable conversation state.
pub mod session;
