//! Environment operation syscall handlers (OP_ENVIRONMENT_*).

use alloc::{boxed::Box, vec::Vec};
use core::{slice, str};

use log::{debug, error, info};

use crate::{
    handle::{Handle, ProcessHandle},
    process::{Process, context::Context},
    resource, scheduler,
};

/// Handle environment open operation.
pub fn handle_open(uri_ptr: usize, uri_len: usize) -> isize {
    let uri_ptr = uri_ptr as *const u8;
    let uri = unsafe { slice::from_raw_parts(uri_ptr, uri_len) };
    let uri = match str::from_utf8(uri) {
        Ok(u) => u,
        Err(_) => return -1,
    };

    match resource::open(uri) {
        Some(file) => {
            let handle_id = scheduler::with_current_process(|proc| {
                proc.handles_mut().insert(Handle::File(file))
            });
            handle_id as isize
        }
        None => -1,
    }
}

/// Handle environment spawn operation.
pub fn handle_spawn(uri_ptr: usize, uri_len: usize) -> isize {
    let uri_ptr = uri_ptr as *const u8;
    let uri = unsafe { slice::from_raw_parts(uri_ptr, uri_len) };
    let uri = match str::from_utf8(uri) {
        Ok(u) => u,
        Err(_) => return -1,
    };

    debug!("SPAWN: uri={}", uri);

    let Some(mut file) = resource::open(uri) else {
        error!("SPAWN: failed to open {}", uri);
        return -1;
    };

    let stat = file.stat();
    let mut elf_data: Vec<u8> = Vec::new();
    elf_data.resize(stat.size as usize, 0);

    match file.read(&mut elf_data) {
        Ok(n) if n == stat.size as usize => {}
        Ok(n) => {
            error!("SPAWN: incomplete read: {} of {} bytes", n, stat.size);
            return -1;
        }
        Err(e) => {
            error!("SPAWN: failed to read {}: {:?}", uri, e);
            return -1;
        }
    }

    let elf_data = elf_data.into_boxed_slice();
    let elf_ptr: *const [u8] = Box::leak(elf_data);

    let process = Process::from_elf_data(Context::new_user_context(), elf_ptr);
    let pid = process.id();
    let process_info = process.info().clone();
    debug!("SPAWN: created process {:?}", pid);

    scheduler::add_process(process);

    // Create a handle for the parent to track the child
    let process_handle = ProcessHandle::new(process_info);
    let handle_id = scheduler::with_current_process(|proc| {
        proc.handles_mut().insert(Handle::Process(process_handle))
    });
    handle_id as isize
}

/// Handle environment log operation.
pub fn handle_log(msg_ptr: usize, msg_len: usize) -> isize {
    let msg_ptr = msg_ptr as *const u8;
    let msg = unsafe { slice::from_raw_parts(msg_ptr, msg_len) };
    let msg = match str::from_utf8(msg) {
        Ok(m) => m,
        Err(_) => return -1,
    };
    info!("LOG: {msg}");
    0
}

/// Handle environment time operation.
pub fn handle_time() -> isize {
    // TODO: Implement getting time
    0
}
