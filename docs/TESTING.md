# Testing guide

This document describes how to write and run tests for the Panda kernel.

## Running tests

```bash
# Run all tests (kernel and userspace)
make test

# Run only kernel tests
make kernel-test

# Run only userspace tests
make userspace-test
```

## Kernel tests

Kernel tests run inside QEMU and test kernel functionality directly. They are located in `panda-kernel/tests/`.

### Writing a kernel test

Create a new file in `panda-kernel/tests/`, for example `panda-kernel/tests/my_feature.rs`:

```rust
#![no_std]
#![no_main]

panda_kernel::test_harness!(test_one, test_two);

fn test_one() {
    assert_eq!(1 + 1, 2);
}

fn test_two() {
    // Test can use kernel APIs
    let boxed = alloc::boxed::Box::new(42);
    assert_eq!(*boxed, 42);
}
```

Key points:
- Use `#![no_std]` and `#![no_main]` attributes
- Use the `test_harness!` macro with a list of test function names
- Each test function takes no arguments and returns nothing
- Use `assert!`, `assert_eq!`, etc. for assertions
- Tests have access to kernel internals including the allocator

### Registering a kernel test

Add your test name to `KERNEL_TESTS` in `Makefile`:

```makefile
KERNEL_TESTS := basic heap pci memory scheduler process nx_bit raii apic my_feature
```

### Example: heap test

```rust
#![no_std]
#![no_main]

extern crate alloc;
use alloc::{boxed::Box, vec::Vec};

panda_kernel::test_harness!(box_allocation, vec_allocation);

fn box_allocation() {
    let boxed = Box::new(42);
    assert_eq!(*boxed, 42);
}

fn vec_allocation() {
    let mut vec = Vec::new();
    for i in 0..100 {
        vec.push(i);
    }
    assert_eq!(vec.len(), 100);
}
```

## Userspace tests

Userspace tests are standalone programs that run as processes on top of the kernel. They are located in `userspace/tests/`.

### Writing a userspace test

1. Create a new crate in `userspace/tests/`:

```bash
mkdir -p userspace/tests/my_test/src
```

2. Create `userspace/tests/my_test/Cargo.toml`:

```toml
[package]
name = "my_test"
version = "0.1.0"
edition = "2024"

[dependencies]
libpanda = { path = "../../libpanda" }
```

3. Create `userspace/tests/my_test/src/main.rs`:

```rust
#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("My test starting");

    // Test logic here...

    if some_condition_failed {
        environment::log("FAIL: something went wrong");
        return 1;  // Non-zero exit code fails the test
    }

    environment::log("My test passed");
    0  // Zero exit code means success
}
```

4. Create `userspace/tests/my_test/expected.txt` with expected log output:

```
# Comments start with #
My test starting
My test passed
```

### Registering a userspace test

Add your test name to `USERSPACE_TESTS` in `Makefile`:

```makefile
USERSPACE_TESTS := vfs_test preempt_test spawn_test yield_test my_test
```

### Tests with multiple binaries

Some tests require additional helper programs (e.g., spawn_test needs spawn_child). Define extras in the Makefile:

```makefile
my_test_EXTRAS := my_helper
export my_test_EXTRAS
```

### Userspace API

Tests use the libpanda API organised by resource type:

```rust
use libpanda::environment;  // System operations
use libpanda::file;         // File operations
use libpanda::process;      // Process operations

// Environment operations (via HANDLE_ENVIRONMENT)
environment::log("message");           // Log to console
environment::open("/path", flags);     // Open file, returns handle
environment::spawn("/path");           // Spawn process
environment::time();                   // Get system time

// File operations (on file handles)
file::read(handle, &mut buf);          // Read from file
file::write(handle, &buf);             // Write to file
file::seek(handle, offset, whence);    // Seek in file
file::stat(handle, &mut stat);         // Get file stats
file::close(handle);                   // Close file

// Process operations (via HANDLE_SELF or child handles)
process::yield_now();                  // Yield CPU
process::exit(code);                   // Exit process
process::getpid();                     // Get process ID
process::wait(child_handle);           // Wait for child
```

### Expected output matching

The test framework extracts log messages and verifies they appear in the expected order.

#### Ordered mode (default)

In the default ordered mode:
- Lines starting with `#` are comments
- Each non-comment line must appear in the log output
- Lines must appear in the specified order
- Additional log messages between expected lines are allowed

Example `expected.txt`:
```
# VFS test checks file operations
VFS test starting
VFS test passed
```

#### Unordered mode with barriers

For tests with non-deterministic output (e.g., concurrent processes), use `# @unordered` mode with `# @barrier` markers:

```
# @unordered
# Patterns within a section can match in any order.
# Use # @barrier to enforce ordering between sections.

First thing that happens
Second thing (order with first doesn't matter)
# @barrier
# Everything above must complete before anything below
Third thing
Fourth thing (order with third doesn't matter)
# @barrier
Final thing that must come last
```

Rules:
- `# @unordered` at the start enables unordered mode
- Patterns within a section can match log lines in any order
- `# @barrier` enforces that all patterns before it match log lines that appear before any patterns after it
- Each pattern still must appear exactly once in the log

**When you need this:** for every pair of adjacent lines in `expected.txt`,
ask whether the *second* line is actually caused by the first — is there a
real synchronization primitive (a channel `recv`, `process::wait`, an event
wait) between them, or could the scheduler just as validly run them in the
other order? If a child process's line isn't gated by receiving something
from the parent first (e.g. it's logged before the child's first blocking
call), its order relative to the parent's most recent log line is **not**
guaranteed — group them with `@unordered`.

This bit a real test: `buffer_owner_test`'s child deliberately does some
work (allocating its own buffer) *before* touching its parent channel at
all, specifically so its log line has no causal relationship to anything
the parent has logged. `expected.txt` assumed a fixed order between the
parent's post-send line and the child's first line anyway. The ordered
matcher discards every log line up through a match once found, so on the
timing where the child's unordered line came out *first* in the raw log,
the matcher's search for the parent's (later) line silently swallowed the
child's line as collateral — surfacing as "expected log not found" for a
line that genuinely was there, just in the "wrong" position. This passed
locally under every KVM-accelerated run and failed on every CI run (which
has no KVM, so uses the much-slower TCG backend with different relative
process-scheduling timing) — a hallmark of this specific bug: **one
specific line missing, every other line from the same process present and
correct, and it never reproduces locally.** See that failure investigation
for the full story if you want the cautionary tale.

Example from `preempt_test/expected.txt`:
```
# @unordered
Preempt test: spawning 3 CPU-bound children
Preempt test: parent doing CPU-bound work
# @barrier
preempt_child: completed
preempt_child: completed
preempt_child: completed
Preempt test: parent work done, waiting for children
# @barrier
Preempt test: all children completed successfully
```

This verifies:
1. Spawning and parent work messages appear first (either order)
2. Then all 3 children complete and parent finishes work (any interleaving)
3. Finally the success message appears last

### Screenshot testing

For GUI tests, you can verify the visual output using screenshot comparison instead of (or in addition to) log matching.

1. Create `userspace/tests/my_test/expected.png` with the expected screenshot.

2. In your test, call `environment::screenshot_ready()` when the display is in the expected state:

```rust
#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    // Set up GUI, draw to surface, etc.
    
    // Signal that the test is ready for screenshot capture
    environment::screenshot_ready();
    
    // The test harness will capture the screenshot and terminate QEMU
    loop {
        core::hint::spin_loop();
    }
}
```

The test harness:
1. Watches for the `<<<SCREENSHOT_READY>>>` marker in the log
2. Captures a screenshot via the QEMU monitor
3. Compares against `expected.png` (with 1% fuzz tolerance for anti-aliasing)
4. Fails if the screenshots differ by more than 1000 pixels

If `expected.png` doesn't exist on the first run, the actual screenshot is saved to `build/utest-<name>/<name>_actual.png` for review. Copy it to `expected.png` if correct.

To update a screenshot after intentional changes:
```bash
cp build/utest-my_test/my_test_actual.png userspace/tests/my_test/expected.png
```

### Expected fault testing

For tests that intentionally trigger a fault (e.g., writing to a read-only page), the test process gets killed by the kernel before it can log any results. To validate that the kernel handled the fault correctly, use `expected_fault.txt` alongside `expected.txt`.

The kernel emits a `<<<PROCESS_FAULT>>>` marker in the error log when it kills a process due to a page fault. This marker only appears when a process is killed for a fault, so it will not appear during normal operation.

1. Create `userspace/tests/my_test/expected_fault.txt` with a single pattern to match the kernel's fault log line:

```
# Verify the kernel detected the fault and killed the process
<<<PROCESS_FAULT>>> Page fault in process
```

2. The test harness extracts all `ERROR:` lines from the QEMU serial output and checks that each pattern in `expected_fault.txt` appears in order (same semantics as ordered `expected.txt`).

3. Use `expected.txt` to verify the test started correctly, and `expected_fault.txt` to verify the kernel's error handling.

**Important:** Each pattern in `expected_fault.txt` matches and consumes a log line. Since the kernel emits the fault details on a single line, use one pattern per fault event rather than splitting across multiple patterns.

This pattern is useful for any test where the expected behaviour is for the kernel to kill the process (protection violations, invalid memory access, etc.).

### Filesystem state testing with debugfs

For tests that modify on-disk filesystem state (e.g., writing to an ext2 partition), you can verify the resulting disk contents after the test completes using `expected_debugfs.txt`. This runs the Linux `debugfs` tool against the test disk image and checks that expected patterns appear in its output.

1. Create `userspace/tests/my_test/expected_debugfs.txt`:

```
# Lines starting with '>' are debugfs commands.
# All other non-comment lines are expected patterns that must appear
# in order in the debugfs output.

# Verify file was written correctly
>cat hello.txt
Hello from Panda OS!

# Verify nested file was modified
>cat subdir/nested.txt
Modified content
```

2. The test harness:
   - Extracts all lines starting with `>` as debugfs commands (the `>` prefix is stripped)
   - Runs them against the test disk image (`test-disk.img`) using `debugfs`
   - Checks that each non-comment, non-command line appears in the debugfs output in order
   - Fails if any expected pattern is missing

**File format:**
- Lines starting with `#` are comments (ignored)
- Lines starting with `>` are debugfs commands (e.g., `>cat hello.txt`, `>ls /`, `>stat file.txt`)
- All other non-blank lines are expected output patterns, matched in order using substring matching
- Patterns are consumed sequentially: after matching pattern N, pattern N+1 is searched for only in the output that follows

**When to use this:** Use `expected_debugfs.txt` when your test writes to a block device and you need to verify the on-disk state is correct. This is particularly useful for filesystem write tests where you want to confirm that data, metadata, and directory structures were persisted correctly. Combine it with `expected.txt` to verify both runtime behaviour (log output) and persistent state (disk contents).

**Requirements:** The test must use a disk image. Tests that need a disk image should be added to the appropriate section in `scripts/setup-userspace-test.sh` and `scripts/run-qemu-test.sh` (for the virtio-blk drive attachment).

Example from `ext2_write_test/expected_debugfs.txt`:
```
# Verify filesystem state after write tests.

# Test 1+2: hello.txt was overwritten and then appended to
>cat hello.txt
Written by Panda OS! Extra data appended.

# Test 4: nested.txt had bytes 7-13 overwritten with "PATCHED"
>cat subdir/nested.txt
Nested PATCHED
```

### Exit codes

- Exit code 0: Test passed
- Exit code non-zero: Test failed (QEMU exits immediately)
- Timeout (60s default): Test failed

### Debugging CI-only test failures

CI runs QEMU without KVM (pure TCG software emulation, no `/dev/kvm` on
GitHub Actions runners), which has meaningfully different scheduling timing
from a local KVM-accelerated run. A failure that only reproduces in CI is
usually not "impossible to debug locally" — it just needs the right theory.
Check causes in roughly this order, cheapest first:

1. **A missing-line, order-only mismatch in `expected.txt`.** Symptom: one
   specific log line is reported missing, while every *other* line from the
   same process is present and in the right relative order to each other.
   This is almost always an unguarded ordering assumption between two lines
   from different processes — see "When you need this" under Unordered mode
   above. Check first: does the "missing" line's process reach it before or
   after any blocking call? If before, and the preceding expected line
   belongs to a *different* process, their relative order isn't guaranteed —
   fix `expected.txt`, don't touch kernel code. This is a five-minute check
   against a multi-hour kernel investigation, so rule it out before assuming
   a concurrency bug in the kernel.
2. **Force TCG locally to get closer to CI's timing.** Temporarily edit the
   `-accel kvm -accel tcg` line in `scripts/run-qemu-test.sh` to `-accel tcg`
   only, then run the failing test (or `make test`) several times in a loop.
   Revert the edit before committing anything — it's a local diagnostic
   change, not a real fix. Note this doesn't always reproduce CI's exact
   failure even when the underlying cause is real and CI-reproducible; a
   clean run under forced TCG is weak evidence of correctness, not proof.
3. **A genuine kernel race**, only after 1 and 2 don't explain it. If you
   land here: reproducing locally (even under forced TCG) is the bar for
   confidence — "I read the code and this looks plausible" is not the same
   as "I watched the bug happen and then watched it stop happening." Say
   explicitly which kind of evidence you actually have when reporting a fix.
4. **Check whether local actually matches CI's environment**, starting with
   the QEMU version. CI runs every step through `nix develop` (see
   `.github/workflows/ci.yml`), the same devShell a local `nix develop`
   gives you, pinned by the committed `flake.lock` — so this should be moot
   today. It wasn't always: CI used to install QEMU via
   `apt-get install qemu-system-x86` (Ubuntu's packaged build, several major
   versions behind the Nix-pinned one), and that gap alone was enough to
   produce genuinely different TCG/timer/virtio timing that no amount of
   matching `-accel tcg` or parallelism locally could reproduce — see
   "Resolved CI-only flakes" below for the full story. If CI's workflow ever
   changes to install tools outside the Nix devShell again, re-check this
   first before trusting a local non-repro.

### Resolved CI-only flakes (postmortems)

Flaky tests are not acceptable long-term — this section is a record of ones
that were found, root-caused, and fixed, kept for pattern-matching the next
one, not a list of tests it's okay to ignore. If a test here starts failing
again, treat it as a regression, not a known quirk.

- **`buffer_transfer_test`** (M2.2 era). Intermittently failed in CI with a
  distinctive symptom: the child's `recv_with_handle` call succeeded, the
  message payload was correct, but the attached buffer handle came back
  `None` even though the parent's `send_with_handle` call had succeeded (the
  attachment genuinely existed at send time). The child then exited without
  signalling, and the parent's follow-up `recv` failed with `PeerClosed` — a
  consequence of the child's earlier failure, not a second independent bug.

  Code review of `resource/channel.rs`'s `send_with_attachment`/
  `recv_with_attachment` and `syscall/channel.rs::handle_recv`'s check →
  register → recheck loop (the pattern from the lost-wakeup fix, `fbf2649`)
  found no bug: message and attachment are pushed/popped together under one
  lock, with no window where they could be split. 50+ local reproduction
  attempts (isolated runs, CI-matched parallelism, CPU-constrained via
  `taskset`) never reproduced it.

  **Root cause: CI and local were never running the same QEMU.** CI installed
  QEMU via `apt-get install qemu-system-x86` (Ubuntu's packaged build);
  every local attempt used the Nix flake's pinned QEMU 11.0.1 — a gap of
  several major versions, big enough on its own to produce genuinely
  different TCG/timer/virtio timing. None of the local non-repro runs were
  ever evidence the kernel code was correct; they only proved the failure
  doesn't reproduce under a QEMU build CI doesn't use.

  **Fix**: `93614d4` switched CI to run every step through `nix develop`,
  resolving to the exact same toolchain (including QEMU) a local `nix
  develop` gives, pinned by the committed `flake.lock`. Confirmed clean
  across 3 consecutive CI runs afterward (previously failing in roughly half
  of all runs) — strong evidence the version gap was the actual cause. This
  also closes the whole *class* of "works locally, fails in CI" problem, not
  just this one instance: local non-repro is now actually meaningful
  evidence again.

## Test infrastructure

### Scripts

- `scripts/run-tests.sh` - Runs multiple tests in parallel
- `scripts/run-qemu-test.sh` - Runs a single test in QEMU
- `scripts/setup-kernel-test.sh` - Prepares kernel test environment
- `scripts/setup-userspace-test.sh` - Prepares userspace test environment

### Build directories

Tests are built to:
- Kernel tests: `build/test-<name>/`
- Userspace tests: `build/utest-<name>/`

Test logs are written to:
- Kernel tests: `build/test-<name>.log`
- Userspace tests: `build/utest-<name>.log`
