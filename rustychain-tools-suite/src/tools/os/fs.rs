//! filesystem
//!
//! This module provides filesystem tools with security controls for agents.
//! Each filesystem operation is implemented as an individual tool.

use rustychain::llm::function::{FnDeclarator, FnExecutor};
use rustychain::{AnyFunction, FunctionDeclaration};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn all_filesystem_tools(config: Arc<FileSystemConfig>) -> Vec<Arc<dyn AnyFunction>> {
    vec![
        Arc::new(ListDirectoryTool::new(config.clone()).declare()),
        Arc::new(ShowTreeTool::new(config.clone()).declare()),
        Arc::new(ReadFileTool::new(config.clone()).declare()),
        Arc::new(WriteFileTool::new(config.clone()).declare()),
        Arc::new(CreateFileTool::new(config.clone()).declare()),
        Arc::new(ConcatenateFilesTool::new(config.clone()).declare()),
        Arc::new(EditFileTool::new(config.clone()).declare()),
        Arc::new(SearchInFileTool::new(config.clone()).declare()),
        Arc::new(CreateDirectoryTool::new(config.clone()).declare()),
        Arc::new(DeleteTool::new(config.clone()).declare()),
        Arc::new(MoveTool::new(config.clone()).declare()),
        Arc::new(CopyTool::new(config.clone()).declare()),
        Arc::new(FileInfoTool::new(config.clone()).declare()),
    ]
}

// ============================================================================
// Security Configuration
// ============================================================================

#[derive(Clone, Debug)]
pub struct FileSystemConfig {
    /// The root directory that restricts all filesystem operations.
    /// If None, filesystem operations are DISABLED for security.
    pub allowed_root: Option<PathBuf>,
    /// Maximum file size in bytes (default: 10MB)
    pub max_file_size: usize,
    /// Maximum number of files to list (default: 1000)
    pub max_list_items: usize,
    /// Allow following symbolic links
    pub follow_symlinks: bool,
    /// Allow operations on hidden files
    pub allow_hidden_files: bool,
    /// Maximum tree depth to prevent excessive recursion
    pub max_tree_depth: usize,
}

impl Default for FileSystemConfig {
    fn default() -> Self {
        Self {
            allowed_root: None,              // Must be explicitly set
            max_file_size: 10 * 1024 * 1024, // 10MB
            max_list_items: 1000,
            follow_symlinks: false,
            allow_hidden_files: false,
            max_tree_depth: 10,
        }
    }
}

impl FileSystemConfig {
    /// Creates a new config with a specific allowed root
    pub fn new(allowed_root: PathBuf) -> Self {
        Self {
            allowed_root: Some(allowed_root),
            ..Default::default()
        }
    }

    pub fn set_allowed_root(mut self, path: PathBuf) -> Self {
        self.allowed_root = Some(path);
        self
    }

    pub fn set_max_file_size(mut self, size: usize) -> Self {
        self.max_file_size = size;
        self
    }

    pub fn set_max_list_items(mut self, count: usize) -> Self {
        self.max_list_items = count;
        self
    }

    pub fn set_follow_symlinks(mut self, follow: bool) -> Self {
        self.follow_symlinks = follow;
        self
    }

    pub fn set_allow_hidden_files(mut self, allow: bool) -> Self {
        self.allow_hidden_files = allow;
        self
    }

    pub fn set_max_tree_depth(mut self, depth: usize) -> Self {
        self.max_tree_depth = depth;
        self
    }

    /// Validates and canonicalizes a path to ensure it's within the allowed root
    fn validate_path(&self, path: &str) -> rustychain::Result<PathBuf> {
        let root = self.allowed_root.as_ref().ok_or_else(|| {
            rustychain::Error::Input(
                "Filesystem operations are disabled: no allowed_root configured".to_string(),
            )
        })?;

        let requested_path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            root.join(path)
        };

        // Check if path is hidden and if that's allowed
        if !self.allow_hidden_files
            && let Some(name) = requested_path.file_name()
            && name.to_string_lossy().starts_with('.')
        {
            return Err(rustychain::Error::Input(
                "Access to hidden files is not allowed".to_string(),
            ));
        }

        let canonical_root = root.canonicalize()?;

        // Handle symlinks
        let canonical_path = if requested_path.exists() {
            if requested_path.is_symlink() && !self.follow_symlinks {
                return Err(rustychain::Error::Input(
                    "Following symbolic links is not allowed".to_string(),
                ));
            }
            requested_path.canonicalize()?
        } else {
            // For non-existent paths, validate the parent directory
            let parent = requested_path.parent().ok_or_else(|| {
                rustychain::Error::Input("Invalid path: no parent directory".to_string())
            })?;

            if parent.exists() {
                let canonical_parent = parent.canonicalize()?;
                canonical_parent.join(requested_path.file_name().unwrap_or_default())
            } else {
                return Err(rustychain::Error::Input(
                    "Parent directory does not exist".to_string(),
                ));
            }
        };

        if !canonical_path.starts_with(&canonical_root) {
            return Err(rustychain::Error::Input(format!(
                "Access denied: path '{}' is outside allowed root '{}'",
                path,
                canonical_root.display()
            )));
        }

        Ok(canonical_path)
    }
}

// ============================================================================
// Tool Arguments
// ============================================================================

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct ListDirectoryArgs {
    #[schemars(description = "The directory path to list. Must be within the allowed root.")]
    pub path: String,
    #[schemars(description = "Include hidden files (starting with .) in the listing.")]
    pub include_hidden: Option<bool>,
    #[schemars(description = "Show detailed metadata (size, modified time, permissions).")]
    pub detailed: Option<bool>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct ShowTreeArgs {
    #[schemars(description = "The directory path to show as a tree.")]
    pub path: String,
    #[schemars(description = "Maximum depth of the tree to display. Default is 3.")]
    pub max_depth: Option<usize>,
    #[schemars(description = "Include hidden files and directories in the tree.")]
    pub include_hidden: Option<bool>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct ReadFileArgs {
    #[schemars(description = "The file path to read.")]
    pub path: String,
    #[schemars(
        description = "Starting line number (1-indexed, inclusive). If not specified, reads from the beginning."
    )]
    pub line_start: Option<usize>,
    #[schemars(
        description = "Ending line number (1-indexed, inclusive). If not specified, reads to the end."
    )]
    pub line_end: Option<usize>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct WriteFileArgs {
    #[schemars(description = "The file path to write to.")]
    pub path: String,
    #[schemars(description = "The content to write to the file.")]
    pub content: String,
    #[schemars(description = "If true, append to the file instead of overwriting.")]
    pub append: Option<bool>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct CreateFileArgs {
    #[schemars(description = "The file path to create.")]
    pub path: String,
    #[schemars(description = "Initial content for the file. Default is empty.")]
    pub content: Option<String>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct ConcatenateFilesArgs {
    #[schemars(description = "List of file paths to concatenate in order.")]
    pub paths: Vec<String>,
    #[schemars(description = "Optional separator to insert between files.")]
    pub separator: Option<String>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct EditFileArgs {
    #[schemars(description = "The file path to edit.")]
    pub path: String,
    #[schemars(description = "The type of edit operation to perform.")]
    pub operation: EditOperation,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EditOperation {
    ReplaceLinesFromColumn {
        #[schemars(description = "Starting line number (1-indexed, inclusive).")]
        start_line: usize,
        #[schemars(description = "Ending line number (1-indexed, inclusive).")]
        end_line: usize,
        #[schemars(description = "Starting column number (1-indexed, inclusive).")]
        start_column: usize,
        #[schemars(description = "New content to replace the lines with.")]
        content: String,
    },
    /// Replace specific lines with new content
    ReplaceLines {
        #[schemars(description = "Starting line number (1-indexed, inclusive).")]
        start: usize,
        #[schemars(description = "Ending line number (1-indexed, inclusive).")]
        end: usize,
        #[schemars(description = "New content to replace the lines with.")]
        content: String,
    },
    /// Insert content at a specific line
    InsertAt {
        #[schemars(
            description = "Line number to insert before (1-indexed). Use 1 to insert at beginning."
        )]
        line: usize,
        #[schemars(description = "Content to insert.")]
        content: String,
    },
    /// Delete specific lines
    DeleteLines {
        #[schemars(description = "Starting line number (1-indexed, inclusive).")]
        start: usize,
        #[schemars(description = "Ending line number (1-indexed, inclusive).")]
        end: usize,
    },
    /// Search and replace text (like find-replace in editors)
    SearchReplace {
        #[schemars(description = "The text pattern to search for.")]
        search: String,
        #[schemars(description = "The replacement text.")]
        replace: String,
        #[schemars(
            description = "If true, replace all occurrences. If false, replace only the first."
        )]
        replace_all: Option<bool>,
        #[schemars(description = "If true, perform case-sensitive search.")]
        case_sensitive: Option<bool>,
    },
    /// Replace entire file content (useful for large refactors)
    ReplaceAll {
        #[schemars(description = "New content for the entire file.")]
        content: String,
    },
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct SearchInFileArgs {
    #[schemars(description = "The file path to search in.")]
    pub path: String,
    #[schemars(description = "The search pattern.")]
    pub pattern: String,
    #[schemars(description = "If true, perform case-sensitive search.")]
    pub case_sensitive: Option<bool>,
    #[schemars(description = "Return context lines around matches.")]
    pub context_lines: Option<usize>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct CreateDirectoryArgs {
    #[schemars(description = "The directory path to create.")]
    pub path: String,
    #[schemars(description = "If true, create parent directories as needed.")]
    pub create_parents: Option<bool>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct DeleteArgs {
    #[schemars(description = "The path to delete (file or directory).")]
    pub path: String,
    #[schemars(description = "If true and path is a directory, delete recursively.")]
    pub recursive: Option<bool>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct MoveArgs {
    #[schemars(description = "Source path.")]
    pub source: String,
    #[schemars(description = "Destination path.")]
    pub destination: String,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct CopyArgs {
    #[schemars(description = "Source path.")]
    pub source: String,
    #[schemars(description = "Destination path.")]
    pub destination: String,
    #[schemars(description = "If true and source is a directory, copy recursively.")]
    pub recursive: Option<bool>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct FileInfoArgs {
    #[schemars(description = "The path to get information about.")]
    pub path: String,
}

// ============================================================================
// Tool Results
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListDirectoryResult {
    pub path: String,
    pub entries: Vec<DirectoryEntry>,
    pub total_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub permissions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeResult {
    pub path: String,
    pub tree: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOperationResult {
    pub success: bool,
    pub message: String,
    pub path: String,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFileResult {
    pub path: String,
    pub content: String,
    pub line_start: usize,
    pub line_end: usize,
    pub total_lines: usize,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcatenateResult {
    pub paths: Vec<String>,
    pub content: String,
    pub total_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchMatch {
    pub line_number: usize,
    pub line_content: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub path: String,
    pub matches: Vec<SearchMatch>,
    pub total_matches: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub is_file: bool,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub accessed: Option<String>,
    pub permissions: String,
    pub total_lines: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditResult {
    pub success: bool,
    pub message: String,
    pub path: String,
    pub lines_affected: usize,
    pub preview: Option<String>,
}

// ============================================================================
// Tool Implementations
// ============================================================================

#[derive(Clone)]
pub struct ListDirectoryTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct ShowTreeTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct ReadFileTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct WriteFileTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct CreateFileTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct ConcatenateFilesTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct EditFileTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct SearchInFileTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct CreateDirectoryTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct DeleteTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct MoveTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct CopyTool {
    pub config: Arc<FileSystemConfig>,
}

#[derive(Clone)]
pub struct FileInfoTool {
    pub config: Arc<FileSystemConfig>,
}

// ============================================================================
// List Directory Tool
// ============================================================================

impl ListDirectoryTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<ListDirectoryArgs, ListDirectoryResult> for ListDirectoryTool {
    async fn call(&self, args: ListDirectoryArgs) -> rustychain::Result<ListDirectoryResult> {
        let path = self.config.validate_path(&args.path)?;

        if !path.is_dir() {
            return Err(rustychain::Error::Input(format!(
                "Path '{}' is not a directory",
                args.path
            )));
        }

        let include_hidden = args
            .include_hidden
            .unwrap_or(self.config.allow_hidden_files);
        let detailed = args.detailed.unwrap_or(false);
        let entries_iter = fs::read_dir(&path)?;

        let mut entries = Vec::new();
        for entry in entries_iter {
            let entry = entry?;

            let name = entry.file_name().to_string_lossy().to_string();

            if !include_hidden && name.starts_with('.') {
                continue;
            }

            let metadata = entry.metadata()?;

            let modified = if detailed {
                metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| format!("{:?}", d))
            } else {
                None
            };

            let permissions = if detailed {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    Some(format!("{:o}", metadata.permissions().mode()))
                }
                #[cfg(not(unix))]
                {
                    Some(format!("readonly={}", metadata.permissions().readonly()))
                }
            } else {
                None
            };

            entries.push(DirectoryEntry {
                name,
                is_directory: metadata.is_dir(),
                is_symlink: metadata.is_symlink(),
                size: if metadata.is_file() {
                    Some(metadata.len())
                } else {
                    None
                },
                modified,
                permissions,
            });

            if entries.len() >= self.config.max_list_items {
                break;
            }
        }

        entries.sort_by(|a, b| match (a.is_directory, b.is_directory) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });

        let total_count = entries.len();
        Ok(ListDirectoryResult {
            path: args.path,
            entries,
            total_count,
        })
    }
}

impl FnDeclarator<ListDirectoryArgs, ListDirectoryResult> for ListDirectoryTool {
    fn declare(&self) -> FunctionDeclaration<ListDirectoryArgs, ListDirectoryResult> {
        FunctionDeclaration {
            name: "list_directory",
            description: "List the contents of a directory. Returns files and subdirectories with their metadata. Similar to 'ls' command.",
            parameters: schema_for!(ListDirectoryArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Show Tree Tool
// ============================================================================

impl ShowTreeTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

fn build_tree(
    path: &Path,
    prefix: &str,
    is_last: bool,
    current_depth: usize,
    max_depth: usize,
    include_hidden: bool,
    output: &mut String,
) -> std::io::Result<()> {
    if current_depth > max_depth {
        return Ok(());
    }

    let name = path.file_name().unwrap().to_string_lossy();

    if !include_hidden && name.starts_with('.') {
        return Ok(());
    }

    let connector = if is_last { "└── " } else { "├── " };
    let suffix = if path.is_dir() { "/" } else { "" };
    output.push_str(&format!("{}{}{}{}\n", prefix, connector, name, suffix));

    if path.is_dir() && current_depth < max_depth {
        let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

        let mut entries: Vec<_> = fs::read_dir(path)?.filter_map(|e| e.ok()).collect();

        entries.sort_by_key(|e| e.path());

        let len = entries.len();
        for (i, entry) in entries.iter().enumerate() {
            let is_last_entry = i == len - 1;
            build_tree(
                &entry.path(),
                &new_prefix,
                is_last_entry,
                current_depth + 1,
                max_depth,
                include_hidden,
                output,
            )?;
        }
    }

    Ok(())
}

#[async_trait::async_trait]
impl FnExecutor<ShowTreeArgs, TreeResult> for ShowTreeTool {
    async fn call(&self, args: ShowTreeArgs) -> rustychain::Result<TreeResult> {
        let path = self.config.validate_path(&args.path)?;

        if !path.is_dir() {
            return Err(rustychain::Error::Input(format!(
                "Path '{}' is not a directory",
                args.path
            )));
        }

        let max_depth = args.max_depth.unwrap_or(3).min(self.config.max_tree_depth);
        let include_hidden = args
            .include_hidden
            .unwrap_or(self.config.allow_hidden_files);

        let mut tree_output = format!("{}/\n", path.display());

        let entries: Vec<_> = fs::read_dir(&path)?.filter_map(|e| e.ok()).collect();

        let len = entries.len();
        for (i, entry) in entries.iter().enumerate() {
            let is_last = i == len - 1;
            build_tree(
                &entry.path(),
                "",
                is_last,
                1,
                max_depth,
                include_hidden,
                &mut tree_output,
            )?;
        }

        Ok(TreeResult {
            path: args.path,
            tree: tree_output,
        })
    }
}

impl FnDeclarator<ShowTreeArgs, TreeResult> for ShowTreeTool {
    fn declare(&self) -> FunctionDeclaration<ShowTreeArgs, TreeResult> {
        FunctionDeclaration {
            name: "show_tree",
            description: "Display a directory structure as a tree. Shows the hierarchical organization of files and directories. Similar to 'tree' command.",
            parameters: schema_for!(ShowTreeArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Read File Tool (with line range support)
// ============================================================================

impl ReadFileTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<ReadFileArgs, ReadFileResult> for ReadFileTool {
    async fn call(&self, args: ReadFileArgs) -> rustychain::Result<ReadFileResult> {
        let path = self.config.validate_path(&args.path)?;

        if !path.is_file() {
            return Err(rustychain::Error::Input(format!(
                "Path '{}' is not a file",
                args.path
            )));
        }

        let metadata = fs::metadata(&path)?;

        if metadata.len() > self.config.max_file_size as u64 {
            return Err(rustychain::Error::Input(format!(
                "File size ({} bytes) exceeds maximum allowed size ({} bytes)",
                metadata.len(),
                self.config.max_file_size
            )));
        }

        let file = fs::File::open(&path)?;

        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<Vec<_>, _>>()?;

        let total_lines = lines.len();

        // Handle line ranges
        let (start_idx, end_idx) = match (args.line_start, args.line_end) {
            (Some(start), Some(end)) => {
                if start == 0 || end == 0 {
                    return Err(rustychain::Error::Input(
                        "Line numbers are 1-indexed and must be greater than 0".to_string(),
                    ));
                }
                if start > end {
                    return Err(rustychain::Error::Input(
                        "line_start must be less than or equal to line_end".to_string(),
                    ));
                }
                // Convert to 0-indexed and clamp to valid range
                let s = (start - 1).min(total_lines);
                let e = end.min(total_lines);
                (s, e)
            }
            (Some(start), None) => {
                if start == 0 {
                    return Err(rustychain::Error::Input(
                        "Line numbers are 1-indexed and must be greater than 0".to_string(),
                    ));
                }
                let s = (start - 1).min(total_lines);
                (s, total_lines)
            }
            (None, Some(end)) => {
                if end == 0 {
                    return Err(rustychain::Error::Input(
                        "Line numbers are 1-indexed and must be greater than 0".to_string(),
                    ));
                }
                let e = end.min(total_lines);
                (0, e)
            }
            (None, None) => (0, total_lines),
        };

        // Extract the requested lines (returns empty string if range is out of bounds)
        let content = if start_idx < total_lines {
            lines[start_idx..end_idx].join("\n")
        } else {
            String::new()
        };

        Ok(ReadFileResult {
            path: args.path,
            content,
            line_start: start_idx + 1, // Convert back to 1-indexed
            line_end: end_idx,         // end_idx is already exclusive, so this is correct
            total_lines,
            size: metadata.len(),
        })
    }
}

impl FnDeclarator<ReadFileArgs, ReadFileResult> for ReadFileTool {
    fn declare(&self) -> FunctionDeclaration<ReadFileArgs, ReadFileResult> {
        FunctionDeclaration {
            name: "read_file",
            description: "Read the contents of a file, optionally specifying a line range. Returns empty string if line range is out of bounds. Line numbers are 1-indexed.",
            parameters: schema_for!(ReadFileArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Write File Tool
// ============================================================================

impl WriteFileTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<WriteFileArgs, FileOperationResult> for WriteFileTool {
    async fn call(&self, args: WriteFileArgs) -> rustychain::Result<FileOperationResult> {
        let path = self.config.validate_path(&args.path)?;

        if args.content.len() > self.config.max_file_size {
            return Err(rustychain::Error::Input(format!(
                "Content size ({} bytes) exceeds maximum allowed size ({} bytes)",
                args.content.len(),
                self.config.max_file_size
            )));
        }

        let mut file = if args.append.unwrap_or(false) {
            fs::OpenOptions::new().create(true).append(true).open(&path)
        } else {
            fs::File::create(&path)
        }?;

        file.write_all(args.content.as_bytes())?;

        let operation = if args.append.unwrap_or(false) {
            "appended to"
        } else {
            "written to"
        };

        Ok(FileOperationResult {
            success: true,
            message: format!("Successfully {} file", operation),
            path: args.path,
            details: Some(serde_json::json!({
                "bytes_written": args.content.len(),
            })),
        })
    }
}

impl FnDeclarator<WriteFileArgs, FileOperationResult> for WriteFileTool {
    fn declare(&self) -> FunctionDeclaration<WriteFileArgs, FileOperationResult> {
        FunctionDeclaration {
            name: "write_file",
            description: "Write content to a file. Can either overwrite existing content or append to it. Creates the file if it doesn't exist.",
            parameters: schema_for!(WriteFileArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Create File Tool
// ============================================================================

impl CreateFileTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<CreateFileArgs, FileOperationResult> for CreateFileTool {
    async fn call(&self, args: CreateFileArgs) -> rustychain::Result<FileOperationResult> {
        let path = self.config.validate_path(&args.path)?;

        if path.exists() {
            return Err(rustychain::Error::Input(format!(
                "File '{}' already exists. Use write_file to overwrite or edit_file to modify.",
                args.path
            )));
        }

        let content = args.content.unwrap_or_default();

        if content.len() > self.config.max_file_size {
            return Err(rustychain::Error::Input(format!(
                "Content size ({} bytes) exceeds maximum allowed size ({} bytes)",
                content.len(),
                self.config.max_file_size
            )));
        }

        let mut file = fs::File::create(&path)?;

        if !content.is_empty() {
            file.write_all(content.as_bytes())?;
        }

        Ok(FileOperationResult {
            success: true,
            message: "Successfully created file".to_string(),
            path: args.path,
            details: Some(serde_json::json!({
                "bytes_written": content.len(),
            })),
        })
    }
}

impl FnDeclarator<CreateFileArgs, FileOperationResult> for CreateFileTool {
    fn declare(&self) -> FunctionDeclaration<CreateFileArgs, FileOperationResult> {
        FunctionDeclaration {
            name: "create_file",
            description: "Create a new file with optional initial content. Fails if the file already exists.",
            parameters: schema_for!(CreateFileArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Concatenate Files Tool
// ============================================================================

impl ConcatenateFilesTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<ConcatenateFilesArgs, ConcatenateResult> for ConcatenateFilesTool {
    async fn call(&self, args: ConcatenateFilesArgs) -> rustychain::Result<ConcatenateResult> {
        let separator = args.separator.unwrap_or_else(|| "\n---\n".to_string());
        let mut concatenated = String::new();
        let mut total_size = 0u64;

        for (i, path_str) in args.paths.iter().enumerate() {
            let path = self.config.validate_path(path_str)?;

            if !path.is_file() {
                return Err(rustychain::Error::Input(format!(
                    "Path '{}' is not a file",
                    path_str
                )));
            }

            let metadata = fs::metadata(&path)?;

            total_size += metadata.len();

            if total_size > self.config.max_file_size as u64 {
                return Err(rustychain::Error::Input(format!(
                    "Total size of files ({} bytes) exceeds maximum allowed size ({} bytes)",
                    total_size, self.config.max_file_size
                )));
            }

            let mut file = fs::File::open(&path)?;

            let mut content = String::new();
            file.read_to_string(&mut content)?;

            if i > 0 {
                concatenated.push_str(&separator);
            }
            concatenated.push_str(&content);
        }

        Ok(ConcatenateResult {
            paths: args.paths,
            content: concatenated,
            total_size,
        })
    }
}

impl FnDeclarator<ConcatenateFilesArgs, ConcatenateResult> for ConcatenateFilesTool {
    fn declare(&self) -> FunctionDeclaration<ConcatenateFilesArgs, ConcatenateResult> {
        FunctionDeclaration {
            name: "concatenate_files",
            description: "Concatenate multiple files into a single response. Files are joined with an optional separator.",
            parameters: schema_for!(ConcatenateFilesArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Edit File Tool (Advanced editor-like operations)
// ============================================================================

impl EditFileTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<EditFileArgs, EditResult> for EditFileTool {
    async fn call(&self, args: EditFileArgs) -> rustychain::Result<EditResult> {
        let path = self.config.validate_path(&args.path)?;

        if !path.is_file() {
            return Err(rustychain::Error::Input(format!(
                "Path '{}' is not a file",
                args.path
            )));
        }

        let file = fs::File::open(&path)?;

        let reader = BufReader::new(file);
        let mut lines: Vec<String> = reader.lines().collect::<Result<Vec<_>, _>>()?;

        let original_line_count = lines.len();
        let mut lines_affected = 0;
        let mut replacements_made = None;

        // Perform the edit operation
        match args.operation {
            EditOperation::ReplaceLinesFromColumn {
                start_line,
                end_line,
                start_column,
                content,
            } => {
                if start_line == 0 || end_line == 0 || start_line > end_line || start_column == 0 {
                    return Err(rustychain::Error::Input(
                        "Invalid line or column range: lines are 1-indexed and start must be <= end; columns are 1-indexed"
                            .to_string(),
                    ));
                }

                let start_idx = (start_line - 1).min(lines.len());
                let end_idx = end_line.min(lines.len());

                if start_idx >= lines.len() {
                    return Err(rustychain::Error::Input(format!(
                        "Start line {} is beyond end of file (total lines: {})",
                        start_line,
                        lines.len()
                    )));
                }

                for line_num in start_idx..end_idx {
                    let line = &mut lines[line_num];
                    let col_idx = (start_column - 1).min(line.len());
                    let new_line = format!(
                        "{}{}",
                        &line[..col_idx],
                        content.lines().next().unwrap_or("")
                    );
                    *line = new_line;
                    lines_affected += 1;
                }
            }
            EditOperation::ReplaceLines {
                start,
                end,
                content,
            } => {
                if start == 0 || end == 0 || start > end {
                    return Err(rustychain::Error::Input(
                        "Invalid line range: lines are 1-indexed and start must be <= end"
                            .to_string(),
                    ));
                }

                let start_idx = (start - 1).min(lines.len());
                let end_idx = end.min(lines.len());

                if start_idx >= lines.len() {
                    return Err(rustychain::Error::Input(format!(
                        "Start line {} is beyond end of file (total lines: {})",
                        start,
                        lines.len()
                    )));
                }

                let new_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
                lines_affected = end_idx - start_idx;
                lines.splice(start_idx..end_idx, new_lines);
            }

            EditOperation::InsertAt { line, content } => {
                if line == 0 {
                    return Err(rustychain::Error::Input(
                        "Invalid line number: lines are 1-indexed".to_string(),
                    ));
                }

                let insert_idx = (line - 1).min(lines.len());
                let new_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
                lines_affected = new_lines.len();

                for (i, new_line) in new_lines.into_iter().enumerate() {
                    lines.insert(insert_idx + i, new_line);
                }
            }

            EditOperation::DeleteLines { start, end } => {
                if start == 0 || end == 0 || start > end {
                    return Err(rustychain::Error::Input(
                        "Invalid line range: lines are 1-indexed and start must be <= end"
                            .to_string(),
                    ));
                }

                let start_idx = (start - 1).min(lines.len());
                let end_idx = end.min(lines.len());

                if start_idx >= lines.len() {
                    return Err(rustychain::Error::Input(format!(
                        "Start line {} is beyond end of file (total lines: {})",
                        start,
                        lines.len()
                    )));
                }

                lines_affected = end_idx - start_idx;
                lines.drain(start_idx..end_idx);
            }

            EditOperation::SearchReplace {
                search,
                replace,
                replace_all,
                case_sensitive,
            } => {
                let replace_all = replace_all.unwrap_or(true);
                let case_sensitive = case_sensitive.unwrap_or(true);
                let mut count = 0;

                for line in lines.iter_mut() {
                    if case_sensitive {
                        if replace_all {
                            let before = line.clone();
                            *line = line.replace(&search, &replace);
                            if *line != before {
                                count += line.matches(&replace).count();
                                lines_affected += 1;
                            }
                        } else if line.contains(&search) {
                            *line = line.replacen(&search, &replace, 1);
                            count += 1;
                            lines_affected += 1;
                            break;
                        }
                    } else {
                        let line_lower = line.to_lowercase();
                        let search_lower = search.to_lowercase();

                        if line_lower.contains(&search_lower) {
                            // Case-insensitive replacement is tricky, we'll do a simple approach
                            let mut result = String::new();
                            let mut last_end = 0;
                            let mut replaced_in_line = false;

                            for (idx, _) in line_lower.match_indices(&search_lower) {
                                result.push_str(&line[last_end..idx]);
                                result.push_str(&replace);
                                last_end = idx + search.len();
                                count += 1;
                                replaced_in_line = true;

                                if !replace_all {
                                    result.push_str(&line[last_end..]);
                                    break;
                                }
                            }

                            if replaced_in_line {
                                if replace_all {
                                    result.push_str(&line[last_end..]);
                                }
                                *line = result;
                                lines_affected += 1;

                                if !replace_all {
                                    break;
                                }
                            }
                        }
                    }
                }

                replacements_made = Some(count);
            }

            EditOperation::ReplaceAll { content } => {
                lines = content.lines().map(|s| s.to_string()).collect();
                lines_affected = lines.len().max(original_line_count);
            }
        }

        let new_content = lines.join("\n");

        if new_content.len() > self.config.max_file_size {
            return Err(rustychain::Error::Input(format!(
                "Edited content size ({} bytes) exceeds maximum allowed size ({} bytes)",
                new_content.len(),
                self.config.max_file_size
            )));
        }

        // Write the file atomically using a temp file
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, &new_content)?;

        fs::rename(&temp_path, &path)?;

        // Generate preview of changes (first 5 lines)
        let preview = if lines.len() <= 5 {
            Some(new_content.clone())
        } else {
            Some(lines.iter().take(5).cloned().collect::<Vec<_>>().join("\n") + "\n...")
        };

        let mut message = "Successfully edited file".to_string();
        if let Some(count) = replacements_made {
            message.push_str(&format!(" ({} replacements made)", count));
        }

        Ok(EditResult {
            success: true,
            message,
            path: args.path,
            lines_affected,
            preview,
        })
    }
}

impl FnDeclarator<EditFileArgs, EditResult> for EditFileTool {
    fn declare(&self) -> FunctionDeclaration<EditFileArgs, EditResult> {
        FunctionDeclaration {
            name: "edit_file",
            description: "Edit an existing file with precise operations: replace lines, insert, delete, search-replace, or replace all. Like a code editor with find-replace, line operations, and refactoring tools.",
            parameters: schema_for!(EditFileArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Search in File Tool
// ============================================================================

impl SearchInFileTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<SearchInFileArgs, SearchResult> for SearchInFileTool {
    async fn call(&self, args: SearchInFileArgs) -> rustychain::Result<SearchResult> {
        let path = self.config.validate_path(&args.path)?;

        if !path.is_file() {
            return Err(rustychain::Error::Input(format!(
                "Path '{}' is not a file",
                args.path
            )));
        }

        let file = fs::File::open(&path)?;

        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<Vec<_>, _>>()?;

        let case_sensitive = args.case_sensitive.unwrap_or(true);
        let context_lines = args.context_lines.unwrap_or(0);
        let mut matches = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            let is_match = if case_sensitive {
                line.contains(&args.pattern)
            } else {
                line.to_lowercase().contains(&args.pattern.to_lowercase())
            };

            if is_match {
                let start_context = i.saturating_sub(context_lines);
                let end_context = (i + context_lines + 1).min(lines.len());

                matches.push(SearchMatch {
                    line_number: i + 1, // 1-indexed
                    line_content: line.clone(),
                    context_before: lines[start_context..i].to_vec(),
                    context_after: lines[i + 1..end_context].to_vec(),
                });
            }
        }

        Ok(SearchResult {
            path: args.path,
            total_matches: matches.len(),
            matches,
        })
    }
}

impl FnDeclarator<SearchInFileArgs, SearchResult> for SearchInFileTool {
    fn declare(&self) -> FunctionDeclaration<SearchInFileArgs, SearchResult> {
        FunctionDeclaration {
            name: "search_in_file",
            description: "Search for a pattern in a file and return all matches with optional context lines. Like grep with context.",
            parameters: schema_for!(SearchInFileArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Create Directory Tool
// ============================================================================

impl CreateDirectoryTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<CreateDirectoryArgs, FileOperationResult> for CreateDirectoryTool {
    async fn call(&self, args: CreateDirectoryArgs) -> rustychain::Result<FileOperationResult> {
        let path = self.config.validate_path(&args.path)?;

        if path.exists() {
            return Err(rustychain::Error::Input(format!(
                "Path '{}' already exists",
                args.path
            )));
        }

        if args.create_parents.unwrap_or(false) {
            fs::create_dir_all(&path)?;
        } else {
            fs::create_dir(&path)?;
        }

        Ok(FileOperationResult {
            success: true,
            message: "Successfully created directory".to_string(),
            path: args.path,
            details: None,
        })
    }
}

impl FnDeclarator<CreateDirectoryArgs, FileOperationResult> for CreateDirectoryTool {
    fn declare(&self) -> FunctionDeclaration<CreateDirectoryArgs, FileOperationResult> {
        FunctionDeclaration {
            name: "create_directory",
            description: "Create a new directory. Can optionally create parent directories if they don't exist. Like 'mkdir' or 'mkdir -p'.",
            parameters: schema_for!(CreateDirectoryArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Delete Tool
// ============================================================================

impl DeleteTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<DeleteArgs, FileOperationResult> for DeleteTool {
    async fn call(&self, args: DeleteArgs) -> rustychain::Result<FileOperationResult> {
        let path = self.config.validate_path(&args.path)?;

        if !path.exists() {
            return Err(rustychain::Error::Input(format!(
                "Path '{}' does not exist",
                args.path
            )));
        }

        if path.is_dir() {
            if args.recursive.unwrap_or(false) {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_dir(&path)?;
            }
        } else {
            fs::remove_file(&path)?;
        }

        Ok(FileOperationResult {
            success: true,
            message: format!(
                "Successfully deleted {}",
                if path.is_dir() { "directory" } else { "file" }
            ),
            path: args.path,
            details: None,
        })
    }
}

impl FnDeclarator<DeleteArgs, FileOperationResult> for DeleteTool {
    fn declare(&self) -> FunctionDeclaration<DeleteArgs, FileOperationResult> {
        FunctionDeclaration {
            name: "delete",
            description: "Delete a file or directory. For directories, use recursive=true to delete non-empty directories. Like 'rm' or 'rm -rf'.",
            parameters: schema_for!(DeleteArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Move Tool
// ============================================================================

impl MoveTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<MoveArgs, FileOperationResult> for MoveTool {
    async fn call(&self, args: MoveArgs) -> rustychain::Result<FileOperationResult> {
        let source_path = self.config.validate_path(&args.source)?;
        let dest_path = self.config.validate_path(&args.destination)?;

        if !source_path.exists() {
            return Err(rustychain::Error::Input(format!(
                "Source path '{}' does not exist",
                args.source
            )));
        }

        if dest_path.exists() {
            return Err(rustychain::Error::Input(format!(
                "Destination path '{}' already exists",
                args.destination
            )));
        }

        fs::rename(&source_path, &dest_path)?;

        Ok(FileOperationResult {
            success: true,
            message: "Successfully moved".to_string(),
            path: args.destination,
            details: Some(serde_json::json!({
                "from": args.source,
            })),
        })
    }
}

impl FnDeclarator<MoveArgs, FileOperationResult> for MoveTool {
    fn declare(&self) -> FunctionDeclaration<MoveArgs, FileOperationResult> {
        FunctionDeclaration {
            name: "move_path",
            description: "Move or rename a file or directory. Like 'mv' command.",
            parameters: schema_for!(MoveArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// Copy Tool
// ============================================================================

impl CopyTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

#[async_trait::async_trait]
impl FnExecutor<CopyArgs, FileOperationResult> for CopyTool {
    async fn call(&self, args: CopyArgs) -> rustychain::Result<FileOperationResult> {
        let source_path = self.config.validate_path(&args.source)?;
        let dest_path = self.config.validate_path(&args.destination)?;

        if !source_path.exists() {
            return Err(rustychain::Error::Input(format!(
                "Source path '{}' does not exist",
                args.source
            )));
        }

        if dest_path.exists() {
            return Err(rustychain::Error::Input(format!(
                "Destination path '{}' already exists",
                args.destination
            )));
        }

        if source_path.is_dir() {
            if args.recursive.unwrap_or(false) {
                copy_dir_recursive(&source_path, &dest_path)?;
            } else {
                return Err(rustychain::Error::Input(
                    "Source is a directory. Use recursive=true to copy directories.".to_string(),
                ));
            }
        } else {
            fs::copy(&source_path, &dest_path)?;
        }

        Ok(FileOperationResult {
            success: true,
            message: "Successfully copied".to_string(),
            path: args.destination,
            details: Some(serde_json::json!({
                "from": args.source,
            })),
        })
    }
}

impl FnDeclarator<CopyArgs, FileOperationResult> for CopyTool {
    fn declare(&self) -> FunctionDeclaration<CopyArgs, FileOperationResult> {
        FunctionDeclaration {
            name: "copy_path",
            description: "Copy a file or directory. For directories, use recursive=true. Like 'cp' or 'cp -r'.",
            parameters: schema_for!(CopyArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// File Info Tool
// ============================================================================

impl FileInfoTool {
    pub fn new(config: Arc<FileSystemConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl FnExecutor<FileInfoArgs, FileInfo> for FileInfoTool {
    async fn call(&self, args: FileInfoArgs) -> rustychain::Result<FileInfo> {
        let path = self.config.validate_path(&args.path)?;

        if !path.exists() {
            return Err(rustychain::Error::Input(format!(
                "Path '{}' does not exist",
                args.path
            )));
        }

        let metadata = fs::metadata(&path)?;

        let created = metadata
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format!("{} seconds since epoch", d.as_secs()));

        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format!("{} seconds since epoch", d.as_secs()));

        let accessed = metadata
            .accessed()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format!("{} seconds since epoch", d.as_secs()));

        let permissions = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                format!("{:o}", metadata.permissions().mode())
            }
            #[cfg(not(unix))]
            {
                format!("readonly={}", metadata.permissions().readonly())
            }
        };

        let total_lines = if metadata.is_file() {
            let file = fs::File::open(&path)?;
            let reader = BufReader::new(file);
            Some(reader.lines().count())
        } else {
            None
        };

        Ok(FileInfo {
            path: args.path,
            size: metadata.len(),
            is_file: metadata.is_file(),
            is_directory: metadata.is_dir(),
            is_symlink: metadata.is_symlink(),
            created,
            modified,
            accessed,
            permissions,
            total_lines,
        })
    }
}

impl FnDeclarator<FileInfoArgs, FileInfo> for FileInfoTool {
    fn declare(&self) -> FunctionDeclaration<FileInfoArgs, FileInfo> {
        FunctionDeclaration {
            name: "get_file_info",
            description: "Get detailed information about a file or directory: size, type, timestamps, permissions, line count. Like 'stat' command.",
            parameters: schema_for!(FileInfoArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
