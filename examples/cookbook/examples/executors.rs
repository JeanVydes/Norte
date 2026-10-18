//! Calling the shipped executors, with no model in the loop.
//!
//! The engine reaches a tool through the registry and its policies, and puts
//! what comes back in a tool result. This recipe is the host calling the same
//! executors directly: each one gets the argument value a model would produce,
//! and the `ToolOutput` the engine would hand back is printed next to what the
//! filesystem and the child process actually did. The tool's word is not the
//! evidence — the file on disk is.
//!
//! ```sh
//! cargo run -p rustychain-cookbook --example executors
//! ```
//!
//! No key and no model: `read`, `write`, `edit` and `command` are host tools.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustychain_core::{FnExecutor, Part, ToolOutput};
use rustychain_tools::{
    CommandRequest, CommandTool, EditRequest, EditSpec, EditTool, LineRange, ReadRequest, ReadTool,
    WriteRequest, WriteTool,
};

/// The value the `command` tool is told to keep out of everything a model sees.
const SECRET: &str = "sk-or-v1-aaaaaaaabbbbbbbbccccccccdddddddd";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let dir = scratch()?;
    println!("scratch directory: {}\n", dir.display());

    let file = dir.join("notes.txt");
    write(&file).await?;
    read(&file).await?;
    edit(&file).await?;
    shell(&dir).await?;
    Ok(())
}

/// A directory of its own; `notes.txt` is rewritten on every run.
fn scratch() -> Result<PathBuf, Box<dyn Error>> {
    let dir = std::env::temp_dir().join("rustychain-cookbook-executors");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Prints what a tool returned, the way the engine would hand it over.
fn show(label: &str, output: &ToolOutput) -> Result<(), Box<dyn Error>> {
    println!("=== {label}");
    for part in &output.parts {
        if let Part::Text(text) = part {
            println!("{}", text.text.trim_end());
        }
    }
    if let Some(structured) = &output.structured {
        println!("{}", serde_json::to_string_pretty(structured)?);
    }
    println!("is_error: {}\n", output.is_error);
    Ok(())
}

/// `write`: a whole file in one call, and then the file the host can read.
async fn write(path: &Path) -> Result<(), Box<dyn Error>> {
    let output = WriteTool::new()
        .call(WriteRequest {
            path: path.to_path_buf(),
            content: "the first line\nthe second line\nthe third line\n".to_owned(),
            overwrite: false,
        })
        .await?;
    show("write", &output)?;
    println!("on disk:\n{}\n", std::fs::read_to_string(path)?);
    Ok(())
}

/// `read`: the whole file, a range, a pattern with context, and the tail.
async fn read(path: &Path) -> Result<(), Box<dyn Error>> {
    let whole = ReadTool::new().call(request(path)).await?;
    show("read (whole file)", &whole)?;

    let range = ReadTool::new()
        .call(ReadRequest {
            lines: Some(LineRange {
                start: 2,
                end: Some(3),
            }),
            ..request(path)
        })
        .await?;
    show("read (lines 2-3)", &range)?;

    let found = ReadTool::new()
        .call(ReadRequest {
            pattern: Some("second".to_owned()),
            context: Some(1),
            ..request(path)
        })
        .await?;
    show("read (pattern, one line of context)", &found)?;

    let tail = ReadTool::new()
        .call(ReadRequest {
            tail: Some(2),
            ..request(path)
        })
        .await?;
    show("read (last two lines)", &tail)?;
    Ok(())
}

/// The read every variant above starts from: the whole file.
fn request(path: &Path) -> ReadRequest {
    ReadRequest {
        path: path.to_path_buf(),
        lines: None,
        tail: None,
        pattern: None,
        context: None,
        invert: false,
        ignore_case: false,
        count_only: false,
        max_matches: None,
    }
}

/// `edit`: a pair that applies, then a pair that cannot.
async fn edit(path: &Path) -> Result<(), Box<dyn Error>> {
    let output = EditTool::new()
        .call(edit_request(path, "the second line", "the corrected line"))
        .await?;
    show("edit", &output)?;
    println!("on disk:\n{}\n", std::fs::read_to_string(path)?);

    let refused = EditTool::new()
        .call(edit_request(path, "a line that is not there", "anything"))
        .await?;
    show("edit (text the file does not contain)", &refused)?;
    Ok(())
}

fn edit_request(path: &Path, search: &str, replace: &str) -> EditRequest {
    EditRequest {
        path: path.to_path_buf(),
        edits: vec![EditSpec {
            search: search.to_owned(),
            replace: replace.to_owned(),
        }],
    }
}

/// `command`: one call per way a process can end, plus the bounds on it.
async fn shell(dir: &Path) -> Result<(), Box<dyn Error>> {
    std::fs::write(dir.join("secret.txt"), SECRET)?;
    let tool = CommandTool::new()
        .with_env(BTreeMap::from([("HOST".to_owned(), "baseline".to_owned())]))
        .with_secrets([SECRET]);

    let failed = tool
        .call(CommandRequest::shell("echo to stdout; echo to stderr >&2; exit 3").with_cwd(dir))
        .await?;
    show("command (exit 3)", &failed)?;

    let timed_out = tool
        .call(
            CommandRequest::shell("sleep 30")
                .with_cwd(dir)
                .with_timeout(Duration::from_millis(300)),
        )
        .await?;
    show("command (timeout)", &timed_out)?;

    let clipped = tool
        .call(
            CommandRequest::shell(
                "i=0; while [ $i -lt 2000 ]; do echo \"line $i\"; i=$((i+1)); done",
            )
            .with_cwd(dir)
            .with_output_cap(4096),
        )
        .await?;
    show("command (more output than the cap)", &clipped)?;

    let redacted = tool
        .call(CommandRequest::shell("cat secret.txt").with_cwd(dir))
        .await?;
    show("command (a secret the host declared)", &redacted)?;

    let environment = tool
        .call(
            CommandRequest::shell("echo \"host=$HOST request=$REQUEST home=${HOME:-unset}\"")
                .with_cwd(dir)
                .with_env("HOST", "from the request")
                .with_env("REQUEST", "added"),
        )
        .await?;
    show("command (environment)", &environment)?;

    let stdin = tool
        .call(
            CommandRequest::shell("cat")
                .with_cwd(dir)
                .with_stdin("piped in\n"),
        )
        .await?;
    show("command (stdin)", &stdin)?;

    let missing = tool
        .call(CommandRequest::program("not-a-real-program-at-all"))
        .await?;
    show("command (a program that does not exist)", &missing)?;

    let signaled = tool
        .call(CommandRequest::shell("kill -TERM $$").with_cwd(dir))
        .await?;
    show("command (killed by a signal)", &signaled)?;
    Ok(())
}
