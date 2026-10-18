#![forbid(unsafe_code)]
#![doc = "Default `RustyChain` tool suite: a command executor and the three file executors."]

mod fs;

pub mod command;
pub mod edit;
pub mod read;
pub mod write;

pub use command::{
    CommandCapabilities, CommandCode, CommandLine, CommandOutput, CommandRequest, CommandTool,
    command_tool,
};
pub use edit::{
    EditCode, EditFailure, EditFailureReason, EditLimits, EditMatch, EditOutcome, EditRequest,
    EditSpec, EditTool, MatchKind, Snippet, edit_tool,
};
pub use read::{
    LineRange, ReadLimits, ReadLine, ReadOutput, ReadRequest, ReadTool, TextKind, read_tool,
};
pub use write::{WriteCode, WriteOutput, WriteRequest, WriteTool, write_tool};
