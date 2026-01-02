use std::{sync::Arc, time::Duration};
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::{
    io::BufReader,
    process::Command,
    sync::{Mutex, mpsc},
    time::timeout,
};

use crate::os::{
    args::{CommandArgs, CommandResult},
    bash::get_user_input_from_terminal,
    input::{InputAction, InputCallback, InputContext, InputDetector},
};

pub struct CommandExecutor {
    pub input_detector: InputDetector,
    pub input_callback: Option<Arc<InputCallback>>,
}

impl CommandExecutor {
    pub fn new(input_detector: InputDetector, input_callback: Option<Arc<InputCallback>>) -> Self {
        Self {
            input_detector,
            input_callback,
        }
    }

    pub fn build_command(&self, args: &CommandArgs) -> Command {
        let mut cmd = if args.use_shell {
            let (shell, shell_arg) = if cfg!(target_os = "windows") {
                ("cmd", "/C")
            } else {
                ("sh", "-c")
            };

            let full_cmd = if args.args.is_empty() {
                args.command.clone()
            } else {
                format!("{} {}", args.command, args.args.join(" "))
            };

            let mut c = Command::new(shell);
            c.arg(shell_arg).arg(full_cmd);
            c
        } else {
            let mut c = Command::new(&args.command);
            c.args(&args.args);
            c
        };

        if let Some(ref dir) = args.working_dir {
            cmd.current_dir(dir);
        }

        for env_var in &args.env {
            cmd.env(&env_var.name, &env_var.value);
        }

        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        cmd
    }

    pub async fn get_input_action(&self, context: InputContext) -> rustychain::Result<InputAction> {
        if let Some(ref callback) = self.input_callback {
            callback(context).await
        } else {
            // Default: ask user via terminal
            let input = get_user_input_from_terminal(&context.prompt).await?;
            Ok(InputAction::Provide(input))
        }
    }

    pub async fn execute(&self, args: CommandArgs) -> rustychain::Result<CommandResult> {
        let start_time = std::time::Instant::now();
        let mut cmd = self.build_command(&args);

        let mut child = cmd.spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| rustychain::Error::Generic("Failed to capture stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| rustychain::Error::Generic("Failed to capture stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| rustychain::Error::Generic("Failed to capture stderr".into()))?;

        let result = self
            .execute_with_io(child, stdin, stdout, stderr, &args, start_time)
            .await?;

        Ok(result)
    }

    async fn execute_with_io(
        &self,
        mut child: tokio::process::Child,
        stdin: tokio::process::ChildStdin,
        stdout: tokio::process::ChildStdout,
        stderr: tokio::process::ChildStderr,
        args: &CommandArgs,
        start_time: std::time::Instant,
    ) -> rustychain::Result<CommandResult> {
        let stdin_writer = Arc::new(Mutex::new(Some(stdin)));
        let stdout_reader = BufReader::new(stdout);
        let stderr_reader = BufReader::new(stderr);

        let full_stdout = Arc::new(Mutex::new(String::new()));
        let full_stderr = Arc::new(Mutex::new(String::new()));
        let cancelled = Arc::new(Mutex::new(Option::<String>::None));

        let (prompt_tx, mut prompt_rx) = mpsc::channel::<(String, String, Option<String>)>(16);

        // Spawn IO reader tasks
        let stdout_handle = self.spawn_reader_task(
            stdout_reader,
            Arc::clone(&full_stdout),
            prompt_tx.clone(),
            "stdout",
            &args.custom_input_patterns,
        );

        let stderr_handle = self.spawn_reader_task(
            stderr_reader,
            Arc::clone(&full_stderr),
            prompt_tx,
            "stderr",
            &args.custom_input_patterns,
        );

        // Main process loop
        let timeout_duration = if args.timeout_secs > 0 {
            Some(Duration::from_secs(args.timeout_secs))
        } else {
            None
        };

        let wait_result = self
            .process_loop(
                &mut child,
                &mut prompt_rx,
                Arc::clone(&stdin_writer),
                Arc::clone(&cancelled),
                Arc::clone(&full_stdout),
                Arc::clone(&full_stderr),
                args,
                start_time,
                timeout_duration,
            )
            .await;

        // Wait for reader tasks
        let _ = stdout_handle.await;
        let _ = stderr_handle.await;

        // Handle timeout
        if wait_result.is_err() {
            let _ = child.kill().await;
            return Ok(CommandResult {
                stdout: full_stdout.lock().await.clone(),
                stderr: full_stderr.lock().await.clone(),
                exit_code: None,
                timed_out: true,
                killed: true,
                duration_ms: start_time.elapsed().as_millis() as u64,
                cancel_reason: None,
            });
        }

        let was_cancelled = cancelled.lock().await.clone();

        match wait_result.unwrap() {
            Ok(status) => Ok(CommandResult {
                stdout: full_stdout.lock().await.clone(),
                stderr: full_stderr.lock().await.clone(),
                exit_code: status.code(),
                timed_out: false,
                killed: was_cancelled.is_some(),
                duration_ms: start_time.elapsed().as_millis() as u64,
                cancel_reason: was_cancelled,
            }),
            Err(e) => Err(rustychain::Error::Generic(format!(
                "Command wait failed: {}",
                e
            ))),
        }
    }

    fn spawn_reader_task(
        &self,
        mut reader: BufReader<impl tokio::io::AsyncRead + Send + Unpin + 'static>,
        output: Arc<Mutex<String>>,
        prompt_tx: mpsc::Sender<(String, String, Option<String>)>,
        source: &'static str,
        custom_patterns: &[String],
    ) -> tokio::task::JoinHandle<()> {
        let detector = self.input_detector.clone();
        let patterns = custom_patterns.to_vec();

        tokio::spawn(async move {
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        output.lock().await.push_str(&line);

                        if let Some(pattern) = detector.find_pattern(&line, &patterns) {
                            let _ = prompt_tx
                                .send((source.to_string(), line, Some(pattern)))
                                .await;
                        }
                    }
                    Err(e) => {
                        log::trace!("{} read error: {}", source, e);
                        break;
                    }
                }
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_loop(
        &self,
        child: &mut tokio::process::Child,
        prompt_rx: &mut mpsc::Receiver<(String, String, Option<String>)>,
        stdin_writer: Arc<Mutex<Option<tokio::process::ChildStdin>>>,
        cancelled: Arc<Mutex<Option<String>>>,
        full_stdout: Arc<Mutex<String>>,
        full_stderr: Arc<Mutex<String>>,
        args: &CommandArgs,
        start_time: std::time::Instant,
        timeout_duration: Option<Duration>,
    ) -> Result<Result<std::process::ExitStatus, std::io::Error>, tokio::time::error::Elapsed> {
        let process_future = async {
            loop {
                tokio::select! {
                    biased;

                    Some((_, line, pattern)) = prompt_rx.recv() => {
                        let context = InputContext {
                            prompt: line.clone(),
                            stdout_so_far: full_stdout.lock().await.clone(),
                            stderr_so_far: full_stderr.lock().await.clone(),
                            command: args.command.clone(),
                            args: args.args.clone(),
                            elapsed_ms: start_time.elapsed().as_millis() as u64,
                            matched_pattern: pattern,
                        };

                        if let Ok(action) = self.get_input_action(context).await {
                            self.handle_input_action(
                                action,
                                &stdin_writer,
                                &cancelled,
                                child,
                            ).await;

                            if cancelled.lock().await.is_some() {
                                break;
                            }
                        }
                    }

                    status = child.wait() => {
                        return status;
                    }
                }
            }

            child.wait().await
        };

        if let Some(dur) = timeout_duration {
            timeout(dur, process_future).await
        } else {
            Ok(process_future.await)
        }
    }

    async fn handle_input_action(
        &self,
        action: InputAction,
        stdin_writer: &Arc<Mutex<Option<tokio::process::ChildStdin>>>,
        cancelled: &Arc<Mutex<Option<String>>>,
        child: &mut tokio::process::Child,
    ) {
        match action {
            InputAction::Provide(input) => {
                let mut guard = stdin_writer.lock().await;
                if let Some(ref mut stdin) = *guard {
                    let input_with_newline = if input.ends_with('\n') {
                        input
                    } else {
                        format!("{}\n", input)
                    };
                    let _ = stdin.write_all(input_with_newline.as_bytes()).await;
                    let _ = stdin.flush().await;
                }
            }
            InputAction::Cancel => {
                *cancelled.lock().await = Some("Cancelled by callback".to_string());
                let _ = child.kill().await;
            }
            InputAction::Skip => {
                // Do nothing    
            }
            InputAction::Interrupt => {
                #[cfg(unix)]
                {
                    if let Some(pid) = child.id() {
                        let _ = std::process::Command::new("kill")
                            .args(["-2", &pid.to_string()])
                            .output();
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = child.kill().await;
                }
            }
            InputAction::SendEof => {
                *stdin_writer.lock().await = None;
            }
        }
    }
}
