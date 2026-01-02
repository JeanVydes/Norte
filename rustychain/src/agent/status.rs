use crate::Inference;

#[derive(Debug, Clone)]
pub enum AgentStatus {
    Idle,
    Executing,
    Finished(Box<Inference>),
    Faulted(String),
}