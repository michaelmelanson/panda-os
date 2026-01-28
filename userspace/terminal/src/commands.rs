//! Command execution and child process handling.

use alloc::string::String;
use alloc::vec::Vec;
use libpanda::{channel, environment, process, process::ChildBuilder, Handle};
use panda_abi::terminal::Request;
use panda_abi::value::Value;
use panda_abi::{EVENT_CHANNEL_READABLE, EVENT_PROCESS_EXITED, MAX_MESSAGE_SIZE};

use crate::Terminal;

/// A single command in a pipeline with its arguments.
struct PipelineStage {
    cmd: String,
    args: Vec<String>,
}

impl Terminal {
    /// Parse the line buffer into pipeline stages.
    ///
    /// Returns None if the line is empty, otherwise returns a list of stages.
    /// Each stage is a (command, args) pair.
    fn parse_pipeline(&self) -> Option<Vec<PipelineStage>> {
        let trimmed = self.line_buffer.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Split by pipe character
        let segments: Vec<&str> = trimmed.split('|').collect();
        let mut stages = Vec::new();

        for segment in segments {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }

            let parts: Vec<&str> = segment.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            let cmd = String::from(parts[0]);
            let args: Vec<String> = parts.iter().map(|s| String::from(*s)).collect();
            stages.push(PipelineStage { cmd, args });
        }

        if stages.is_empty() {
            None
        } else {
            Some(stages)
        }
    }

    /// Parse the line buffer into command and arguments (legacy single command).
    pub fn parse_command(&self) -> Option<(String, Vec<String>)> {
        let trimmed = self.line_buffer.trim();
        if trimmed.is_empty() {
            return None;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let cmd = String::from(parts[0]);
        let args: Vec<String> = parts.iter().map(|s| String::from(*s)).collect();
        Some((cmd, args))
    }

    /// Resolve a command name to an executable path.
    pub fn resolve_command(&self, cmd: &str) -> Option<String> {
        if cmd.contains('/') {
            return Some(alloc::format!("file:{}", cmd));
        }

        // Try /mnt first (ext2 filesystem)
        let mnt_path = alloc::format!("file:/mnt/{}", cmd);
        if environment::stat(&mnt_path).is_ok() {
            return Some(mnt_path);
        }

        // Try /initrd
        let initrd_path = alloc::format!("file:/initrd/{}", cmd);
        if environment::stat(&initrd_path).is_ok() {
            return Some(initrd_path);
        }

        None
    }

    /// Execute a command (handles both single commands and pipelines).
    pub fn execute_command(&mut self) {
        let Some(stages) = self.parse_pipeline() else {
            return;
        };

        // Handle built-in commands (only for single-stage pipelines)
        if stages.len() == 1 {
            match stages[0].cmd.as_str() {
                "clear" => {
                    self.clear();
                    return;
                }
                "exit" => {
                    process::exit(0);
                }
                _ => {}
            }
        }

        if stages.len() == 1 {
            // Single command - use simple spawn
            self.execute_single_command(&stages[0]);
        } else {
            // Pipeline - spawn multiple processes with channels
            self.execute_pipeline(stages);
        }
    }

    /// Execute a single command (no pipeline).
    fn execute_single_command(&mut self, stage: &PipelineStage) {
        let Some(path) = self.resolve_command(&stage.cmd) else {
            self.write_line(&alloc::format!("{}: command not found", stage.cmd));
            return;
        };

        let arg_refs: Vec<&str> = stage.args.iter().map(|s| s.as_str()).collect();
        let events = EVENT_PROCESS_EXITED | EVENT_CHANNEL_READABLE;

        match ChildBuilder::new(&path)
            .args(&arg_refs)
            .mailbox(self.mailbox.handle(), events)
            .spawn_handle()
        {
            Ok(child_handle) => {
                self.child = Some(child_handle);
                self.pipeline_children.clear();
            }
            Err(_) => {
                self.write_line(&alloc::format!("{}: failed to execute", stage.cmd));
            }
        }
    }

    /// Execute a pipeline of commands.
    fn execute_pipeline(&mut self, stages: Vec<PipelineStage>) {
        // Clear any existing children
        self.child = None;
        self.pipeline_children.clear();

        let n = stages.len();
        if n == 0 {
            return;
        }

        // Create channels between stages
        // For n stages, we need n-1 channels
        let mut channels: Vec<(Handle, Handle)> = Vec::new();
        for _ in 0..(n - 1) {
            let Ok((a, b)) = channel::create_pair() else {
                self.write_line("failed to create pipeline channel");
                return;
            };
            channels.push((a.into(), b.into()));
        }

        let events = EVENT_PROCESS_EXITED | EVENT_CHANNEL_READABLE;

        // Spawn each stage
        for (i, stage) in stages.iter().enumerate() {
            let Some(path) = self.resolve_command(&stage.cmd) else {
                self.write_line(&alloc::format!("{}: command not found", stage.cmd));
                // Clean up already spawned processes
                for handle in &self.pipeline_children {
                    // Best effort cleanup
                    let _ = process::wait(*handle);
                }
                self.pipeline_children.clear();
                return;
            };

            let arg_refs: Vec<&str> = stage.args.iter().map(|s| s.as_str()).collect();

            // Build spawn command with optional stdin/stdout redirection
            let mut builder = ChildBuilder::new(&path)
                .args(&arg_refs)
                .mailbox(self.mailbox.handle(), events);

            // First stage: no stdin redirection
            // Middle/last stages: use read end of previous channel
            if i > 0 {
                builder = builder.stdin(channels[i - 1].0);
            }

            // Last stage: no stdout redirection (output goes to terminal)
            // First/middle stages: use write end of next channel
            if i < n - 1 {
                builder = builder.stdout(channels[i].1);
            }

            match builder.spawn_handle() {
                Ok(child_handle) => {
                    self.pipeline_children.push(child_handle);
                }
                Err(_) => {
                    self.write_line(&alloc::format!("{}: failed to execute", stage.cmd));
                    // Clean up already spawned processes
                    for handle in &self.pipeline_children {
                        let _ = process::wait(*handle);
                    }
                    self.pipeline_children.clear();
                    return;
                }
            }
        }

        // The last child is the "main" child for output purposes
        if let Some(last) = self.pipeline_children.last() {
            self.child = Some(*last);
        }
    }

    /// Handle child process exit.
    pub fn handle_child_exit(&mut self, handle: Handle) {
        // Check if it's the main child
        if let Some(child) = self.child {
            if child.as_raw() == handle.as_raw() {
                let exit_code = process::wait(child);
                if exit_code != 0 {
                    self.write_line(&alloc::format!("(exited with code {})", exit_code));
                }
                self.child = None;
                self.pending_input = None;
            }
        }

        // Also check pipeline children
        self.pipeline_children.retain(|&h: &Handle| {
            if h.as_raw() == handle.as_raw() {
                let _ = process::wait(h);
                false // Remove from list
            } else {
                true // Keep in list
            }
        });
    }

    /// Process channel messages from child.
    pub fn process_child_messages(&mut self, handle: Handle) {
        let mut buf = [0u8; MAX_MESSAGE_SIZE];

        loop {
            match channel::try_recv(handle, &mut buf) {
                Ok(len) if len > 0 => {
                    // Try to parse as Request first
                    if let Ok((msg, _)) = Request::from_bytes(&buf[..len]) {
                        self.handle_request(msg, handle);
                    } else if let Ok(value) = Value::from_bytes(&buf[..len]) {
                        // Raw Value from pipeline output
                        self.render_value(&value);
                        self.flush();
                    }
                }
                _ => break,
            }
        }
    }
}
