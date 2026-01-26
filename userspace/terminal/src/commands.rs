//! Command execution and child process handling.

use alloc::string::String;
use alloc::vec::Vec;
use libpanda::{channel, environment, process, Handle};
use panda_abi::terminal::TerminalOutput;
use panda_abi::{EVENT_CHANNEL_READABLE, EVENT_PROCESS_EXITED, MAX_MESSAGE_SIZE};

use crate::Terminal;

impl Terminal {
    /// Parse the line buffer into command and arguments
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

    /// Resolve a command name to an executable path
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

    /// Execute a command
    pub fn execute_command(&mut self) {
        let Some((cmd, args)) = self.parse_command() else {
            return;
        };

        // Handle built-in commands
        match cmd.as_str() {
            "clear" => {
                self.clear();
                return;
            }
            "exit" => {
                process::exit(0);
            }
            _ => {}
        }

        // Resolve command to executable path
        let Some(path) = self.resolve_command(&cmd) else {
            self.write_line(&alloc::format!("{}: command not found", cmd));
            return;
        };

        // Convert args to &str slice for spawn
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        // Spawn the process with mailbox attachment for events
        // We want both channel readable (for IPC) and process exited events
        let events = EVENT_PROCESS_EXITED | EVENT_CHANNEL_READABLE;
        match environment::spawn(&path, &arg_refs, self.mailbox.handle().as_raw(), events) {
            Ok(child_handle) => {
                self.child = Some(child_handle);
            }
            Err(_) => {
                self.write_line(&alloc::format!("{}: failed to execute", cmd));
            }
        }
    }

    /// Handle child process exit
    pub fn handle_child_exit(&mut self, handle: Handle) {
        if let Some(child) = self.child.take() {
            if child.as_raw() == handle.as_raw() {
                let exit_code = process::wait(child);
                if exit_code != 0 {
                    self.write_line(&alloc::format!("(exited with code {})", exit_code));
                }
                // Clear any pending input state
                self.pending_input = None;
            }
        }
    }

    /// Process channel messages from child
    pub fn process_child_messages(&mut self, handle: Handle) {
        let mut buf = [0u8; MAX_MESSAGE_SIZE];

        loop {
            match channel::try_recv(handle, &mut buf) {
                Ok(len) if len > 0 => {
                    if let Ok((msg, _)) = TerminalOutput::from_bytes(&buf[..len]) {
                        self.handle_terminal_output(msg, handle);
                    }
                }
                _ => break,
            }
        }
    }
}
