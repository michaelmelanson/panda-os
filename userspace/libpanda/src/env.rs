//! Environment variable support.
//!
//! Environment variables are key-value pairs inherited from the parent process.
//! They can be read and modified at runtime, and are passed to child processes.
//!
//! # Example
//!
//! ```
//! use libpanda::env;
//!
//! // Get a variable
//! if let Some(path) = env::get("PATH") {
//!     // use path
//! }
//!
//! // Set a variable (affects this process and children)
//! env::set("MY_VAR", "value");
//!
//! // Get all variables
//! for (key, value) in env::vars() {
//!     // ...
//! }
//! ```

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;

/// Global environment storage.
///
/// Safety: Panda userspace is single-threaded, so this is safe.
struct EnvStorage {
    inner: UnsafeCell<Option<Vec<(String, String)>>>,
}

unsafe impl Sync for EnvStorage {}

impl EnvStorage {
    const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(None),
        }
    }

    fn get(&self) -> &Vec<(String, String)> {
        // Safety: single-threaded userspace
        unsafe { (*self.inner.get()).as_ref().unwrap_or(&EMPTY_ENV) }
    }

    fn get_mut(&self) -> &mut Vec<(String, String)> {
        // Safety: single-threaded userspace
        unsafe {
            let inner = &mut *self.inner.get();
            if inner.is_none() {
                *inner = Some(Vec::new());
            }
            inner.as_mut().unwrap()
        }
    }

    fn init(&self, env: Vec<(String, String)>) {
        // Safety: single-threaded userspace
        unsafe {
            *self.inner.get() = Some(env);
        }
    }
}

static ENV: EnvStorage = EnvStorage::new();
static EMPTY_ENV: Vec<(String, String)> = Vec::new();

/// Initialise the environment from the startup message.
///
/// This is called by the `main!` macro during startup. User code should not
/// call this directly.
pub fn init(env: Vec<(String, String)>) {
    ENV.init(env);
}

/// Get the value of an environment variable.
///
/// Returns `None` if the variable is not set.
pub fn get(key: &str) -> Option<String> {
    for (k, v) in ENV.get().iter() {
        if k == key {
            return Some(v.clone());
        }
    }
    None
}

/// Set an environment variable.
///
/// If the variable already exists, its value is updated.
/// If it doesn't exist, a new variable is created.
pub fn set(key: &str, value: &str) {
    let env = ENV.get_mut();
    for (k, v) in env.iter_mut() {
        if k == key {
            *v = String::from(value);
            return;
        }
    }
    env.push((String::from(key), String::from(value)));
}

/// Remove an environment variable.
///
/// Returns the previous value if the variable existed.
pub fn remove(key: &str) -> Option<String> {
    let env = ENV.get_mut();
    if let Some(pos) = env.iter().position(|(k, _)| k == key) {
        let (_, value) = env.remove(pos);
        Some(value)
    } else {
        None
    }
}

/// Get all environment variables.
///
/// Returns a vector of (key, value) pairs.
pub fn vars() -> Vec<(String, String)> {
    ENV.get().clone()
}
