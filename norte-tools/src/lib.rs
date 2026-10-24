#![forbid(unsafe_code)]
#![doc = "Default `Norte` tool suite: a command executor and the three file executors."]

mod fs;
mod text;

pub mod browser;
pub mod command;
pub mod edit;
pub mod read;
pub mod write;

pub use browser::{
    BrowserAction, BrowserLimits, BrowserOutput, BrowserRequest, BrowserTool, InteractiveElement,
    browser_tool,
};

pub use command::{
    CommandCode, CommandLimits, CommandLine, CommandOutput, CommandRenderer, CommandRequest,
    CommandTool, PlainCommandRenderer, command_tool,
};
pub use edit::{
    EditCode, EditFailure, EditFailureReason, EditLimits, EditMatch, EditOutcome, EditRequest,
    EditSpec, EditTool, MatchKind, Snippet, edit_tool,
};
pub use read::{
    LineRange, ReadLimits, ReadLine, ReadOutput, ReadRequest, ReadTool, TextKind, read_tool,
};
pub use write::{WriteCode, WriteOutput, WriteRequest, WriteTool, write_tool};
