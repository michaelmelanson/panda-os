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
use spinning_top::RwSpinlock;

/// Global environment storage protected by a read-write spinlock.
static ENV: RwSpinlock<Vec<(String, String)>> = RwSpinlock::new(Vec::new());

/// Initialise the environment from the startup message.
///
/// This is called by the `main!` macro during startup. User code should not
/// call this directly.
pub fn init(env: Vec<(String, String)>) {
    *ENV.write() = env;
}

/// Get the value of an environment variable.
///
/// Returns `None` if the variable is not set.
pub fn get(key: &str) -> Option<String> {
    let env = ENV.read();
    for (k, v) in env.iter() {
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
    let mut env = ENV.write();
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
    let mut env = ENV.write();
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
    ENV.read().clone()
}
