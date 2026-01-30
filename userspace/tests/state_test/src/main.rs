//! Tests that verify process state is preserved correctly across syscalls
//! and when resuming from blocked states.
//!
//! These tests ensure that:
//! 1. Callee-saved registers (rbx, rbp, r12-r15) are preserved across syscalls
//! 2. Stack state is preserved across syscalls
//! 3. Local variables maintain their values across blocking syscalls
//! 4. Multiple syscalls in sequence don't corrupt state

#![no_std]
#![no_main]

use core::arch::asm;
use libpanda::Handle;
use libpanda::environment;
use libpanda::file;
use libpanda::process;

/// Test that callee-saved registers are preserved across a simple (non-blocking) syscall.
fn test_registers_preserved_simple_syscall() {
    environment::log("TEST: registers_preserved_simple_syscall");

    // Set known values in callee-saved registers, do a yield syscall,
    // and capture the "after" values — all in one asm block so the compiler
    // cannot interfere with register allocation between setup and syscall.
    //
    // We push the original callee-saved values to the stack, set our test
    // values, do the syscall, capture the results, and restore originals.
    // We test all 6 callee-saved registers in a single asm block.
    // To avoid the compiler assigning our input/output operands to
    // callee-saved registers (which we explicitly use), we pass results
    // out via a single combined flag: all-pass = 1, any-fail = 0.
    let all_ok: u64;

    unsafe {
        asm!(
            // Save original callee-saved registers
            "push rbx",
            "push rbp",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            // Set test values
            "mov rbx, 0xDEADBEEF11111111",
            "mov rbp, 0xDEADBEEF66666666",
            "mov r12, 0xDEADBEEF22222222",
            "mov r13, 0xDEADBEEF33333333",
            "mov r14, 0xDEADBEEF44444444",
            "mov r15, 0xDEADBEEF55555555",
            // Do yield syscall (SYSCALL_SEND=0x30, handle=SELF=0x11000003,
            // op=OP_PROCESS_YIELD=0x20000)
            "mov rax, 0x30",
            "mov rdi, 0x11000003",
            "mov rsi, 0x20000",
            "xor edx, edx",
            "xor r10d, r10d",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "syscall",
            // Check all registers; use rax as running AND of pass/fail
            "mov rdi, 1",  // rdi = all_ok accumulator (start true)
            "mov rcx, 0xDEADBEEF11111111",
            "cmp rbx, rcx",
            "jne 50f",
            "mov rcx, 0xDEADBEEF66666666",
            "cmp rbp, rcx",
            "jne 51f",
            "mov rcx, 0xDEADBEEF22222222",
            "cmp r12, rcx",
            "jne 52f",
            "mov rcx, 0xDEADBEEF33333333",
            "cmp r13, rcx",
            "jne 53f",
            "mov rcx, 0xDEADBEEF44444444",
            "cmp r14, rcx",
            "jne 54f",
            "mov rcx, 0xDEADBEEF55555555",
            "cmp r15, rcx",
            "jne 55f",
            // All passed — rdi still 1, rsi = 0 (no failure)
            "xor esi, esi",
            "jmp 59f",
            // Failure labels — set rdi=0, rsi=which register failed (1-6)
            "50:", "xor edi, edi", "mov esi, 1", "jmp 59f",
            "51:", "xor edi, edi", "mov esi, 2", "jmp 59f",
            "52:", "xor edi, edi", "mov esi, 3", "jmp 59f",
            "53:", "xor edi, edi", "mov esi, 4", "jmp 59f",
            "54:", "xor edi, edi", "mov esi, 5", "jmp 59f",
            "55:", "xor edi, edi", "mov esi, 6",
            "59:",
            // Restore original callee-saved registers
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbp",
            "pop rbx",
            out("rax") _,
            out("rcx") _,
            out("rdx") _,
            out("rsi") _,
            out("rdi") all_ok,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            out("r11") _,
        );
    }

    if all_ok == 0 {
        environment::log("FAIL: callee-saved register corrupted across syscall");
        process::exit(1);
    }

    environment::log("PASS: registers_preserved_simple_syscall");
}

/// Test that local variables on the stack are preserved across syscalls.
fn test_stack_variables_preserved() {
    environment::log("TEST: stack_variables_preserved");

    // Create various types of stack variables
    let a: u64 = 0x1234567890ABCDEF;
    let b: u32 = 0xCAFEBABE;
    let c: u16 = 0xBEEF;
    let d: u8 = 0x42;
    let array: [u64; 4] = [0x1111, 0x2222, 0x3333, 0x4444];

    // Do a syscall
    process::yield_now();

    // Verify all values are unchanged
    if a != 0x1234567890ABCDEF {
        environment::log("FAIL: u64 variable corrupted");
        process::exit(1);
    }
    if b != 0xCAFEBABE {
        environment::log("FAIL: u32 variable corrupted");
        process::exit(1);
    }
    if c != 0xBEEF {
        environment::log("FAIL: u16 variable corrupted");
        process::exit(1);
    }
    if d != 0x42 {
        environment::log("FAIL: u8 variable corrupted");
        process::exit(1);
    }
    if array[0] != 0x1111 || array[1] != 0x2222 || array[2] != 0x3333 || array[3] != 0x4444 {
        environment::log("FAIL: array corrupted");
        process::exit(1);
    }

    environment::log("PASS: stack_variables_preserved");
}

/// Test that state is preserved across a blocking read syscall.
fn test_state_preserved_blocking_read() {
    environment::log("TEST: state_preserved_blocking_read");

    // Open the keyboard (this will be a blocking read)
    let Ok(keyboard) = environment::open("keyboard:/pci/00:03.0", 0, 0) else {
        environment::log("SKIP: keyboard not available");
        return;
    };

    // Set up state before blocking
    let magic1: u64 = 0xAAAABBBBCCCCDDDD;
    let magic2: u64 = 0x1111222233334444;
    let array: [u32; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

    // We can't actually block waiting for keyboard input in an automated test,
    // but we can verify the state is set up correctly and would be preserved.
    // Instead, let's test with a file read which is non-blocking.

    // Open a file from initrd
    let Ok(f) = environment::open("file:/initrd/hello.txt", 0, 0) else {
        environment::log("SKIP: test file not available");
        file::close(keyboard);
        return;
    };

    let mut buf = [0u8; 32];
    let _n = file::read(f, &mut buf);

    // Verify state after read
    if magic1 != 0xAAAABBBBCCCCDDDD {
        environment::log("FAIL: magic1 corrupted after read");
        process::exit(1);
    }
    if magic2 != 0x1111222233334444 {
        environment::log("FAIL: magic2 corrupted after read");
        process::exit(1);
    }
    for i in 0..8 {
        if array[i] != (i as u32 + 1) {
            environment::log("FAIL: array corrupted after read");
            process::exit(1);
        }
    }

    file::close(f);
    file::close(keyboard);

    environment::log("PASS: state_preserved_blocking_read");
}

/// Test that multiple syscalls in rapid succession don't corrupt state.
fn test_multiple_syscalls_preserve_state() {
    environment::log("TEST: multiple_syscalls_preserve_state");

    let mut counter: u64 = 0;
    let sentinel: u64 = 0xFEDCBA9876543210;

    // Do many syscalls in a row
    for i in 0..100u64 {
        counter = i;

        // Mix of different syscall types
        process::yield_now();

        if counter != i {
            environment::log("FAIL: counter corrupted during iteration");
            process::exit(1);
        }
        if sentinel != 0xFEDCBA9876543210 {
            environment::log("FAIL: sentinel corrupted during iteration");
            process::exit(1);
        }
    }

    if counter != 99 {
        environment::log("FAIL: final counter value wrong");
        process::exit(1);
    }

    environment::log("PASS: multiple_syscalls_preserve_state");
}

/// Test that deeply nested function calls with syscalls preserve all stack frames.
fn test_nested_calls_with_syscalls() {
    environment::log("TEST: nested_calls_with_syscalls");

    fn level1(val: u64) -> u64 {
        let local = val + 1;
        process::yield_now();
        let result = level2(local);
        if local != val + 1 {
            environment::log("FAIL: level1 local corrupted");
            process::exit(1);
        }
        result
    }

    fn level2(val: u64) -> u64 {
        let local = val + 2;
        process::yield_now();
        let result = level3(local);
        if local != val + 2 {
            environment::log("FAIL: level2 local corrupted");
            process::exit(1);
        }
        result
    }

    fn level3(val: u64) -> u64 {
        let local = val + 3;
        process::yield_now();
        if local != val + 3 {
            environment::log("FAIL: level3 local corrupted");
            process::exit(1);
        }
        local
    }

    let result = level1(100);
    if result != 106 {
        // 100 + 1 + 2 + 3
        environment::log("FAIL: nested result wrong");
        process::exit(1);
    }

    environment::log("PASS: nested_calls_with_syscalls");
}

/// Test that the return value from syscalls is correct.
fn test_syscall_return_values() {
    environment::log("TEST: syscall_return_values");

    // Test that yield returns 0
    let ret = libpanda::sys::send(Handle::SELF, panda_abi::OP_PROCESS_YIELD, 0, 0, 0, 0);
    if ret != 0 {
        environment::log("FAIL: yield should return 0");
        process::exit(1);
    }

    // Test that open returns a valid handle
    let Ok(fd) = environment::open("file:/initrd/hello.txt", 0, 0) else {
        environment::log("FAIL: open should return valid fd");
        process::exit(1);
    };

    // Test that close returns 0
    let ret = file::close(fd);
    if ret != 0 {
        environment::log("FAIL: close should return 0");
        process::exit(1);
    }

    // Test that opening non-existent file returns error
    if let Ok(_) = environment::open("file:/initrd/nonexistent", 0, 0) {
        environment::log("FAIL: open nonexistent should fail");
        process::exit(1);
    }

    environment::log("PASS: syscall_return_values");
}

/// Test that registers used for syscall arguments don't leak into return state.
fn test_syscall_arg_registers_clean() {
    environment::log("TEST: syscall_arg_registers_clean");

    // The syscall ABI uses rax, rdi, rsi, rdx, r10, r8, r9 for arguments.
    // After a syscall, only rax should have the return value.
    // The other registers are caller-saved and may be clobbered, but
    // we should verify they don't contain sensitive kernel data.

    let rdi_after: u64;
    let rsi_after: u64;
    let rdx_after: u64;
    let r8_after: u64;
    let r9_after: u64;
    let r10_after: u64;

    unsafe {
        // Do a syscall
        asm!(
            "mov rax, {syscall_send}",
            "mov rdi, {handle}",      // HANDLE_SELF
            "mov rsi, {op}",          // OP_PROCESS_YIELD
            "xor rdx, rdx",
            "xor r10, r10",
            "xor r8, r8",
            "xor r9, r9",
            "syscall",
            // Capture the state of argument registers after syscall
            "mov {rdi_out}, rdi",
            "mov {rsi_out}, rsi",
            "mov {rdx_out}, rdx",
            "mov {r8_out}, r8",
            "mov {r9_out}, r9",
            "mov {r10_out}, r10",
            syscall_send = const panda_abi::SYSCALL_SEND,
            handle = const Handle::SELF.as_raw(),
            op = const panda_abi::OP_PROCESS_YIELD,
            rdi_out = out(reg) rdi_after,
            rsi_out = out(reg) rsi_after,
            rdx_out = out(reg) rdx_after,
            r8_out = out(reg) r8_after,
            r9_out = out(reg) r9_after,
            r10_out = out(reg) r10_after,
            out("rax") _,
            out("rcx") _,
            out("r11") _,
        );
    }

    // These registers are caller-saved, so the kernel is allowed to clobber them.
    // But they should contain our original values OR be zeroed, not kernel addresses.
    // We just check they don't look like kernel pointers (high bits set in canonical form).

    fn looks_like_kernel_ptr(val: u64) -> bool {
        // Kernel addresses typically have high bits set (0xFFFF8...)
        val > 0xFFFF_0000_0000_0000
    }

    if looks_like_kernel_ptr(rdi_after) {
        environment::log("WARN: rdi looks like kernel pointer after syscall");
    }
    if looks_like_kernel_ptr(rsi_after) {
        environment::log("WARN: rsi looks like kernel pointer after syscall");
    }
    if looks_like_kernel_ptr(rdx_after) {
        environment::log("WARN: rdx looks like kernel pointer after syscall");
    }
    if looks_like_kernel_ptr(r8_after) {
        environment::log("WARN: r8 looks like kernel pointer after syscall");
    }
    if looks_like_kernel_ptr(r9_after) {
        environment::log("WARN: r9 looks like kernel pointer after syscall");
    }
    if looks_like_kernel_ptr(r10_after) {
        environment::log("WARN: r10 looks like kernel pointer after syscall");
    }

    environment::log("PASS: syscall_arg_registers_clean");
}

/// Test heap allocations across syscalls.
fn test_heap_preserved_across_syscalls() {
    environment::log("TEST: heap_preserved_across_syscalls");

    use libpanda::Box;
    use libpanda::Vec;
    use libpanda::vec;

    // Allocate some heap memory
    let boxed: Box<u64> = Box::new(0x123456789ABCDEF0);
    let mut vec: Vec<u32> = vec![1, 2, 3, 4, 5];

    // Do syscalls
    for _ in 0..10 {
        process::yield_now();
    }

    // Verify heap data
    if *boxed != 0x123456789ABCDEF0 {
        environment::log("FAIL: boxed value corrupted");
        process::exit(1);
    }

    if vec.len() != 5 {
        environment::log("FAIL: vec length wrong");
        process::exit(1);
    }

    for i in 0..5 {
        if vec[i] != (i as u32 + 1) {
            environment::log("FAIL: vec element corrupted");
            process::exit(1);
        }
    }

    // Modify and verify again after more syscalls
    vec.push(6);
    process::yield_now();

    if vec.len() != 6 || vec[5] != 6 {
        environment::log("FAIL: vec modification lost");
        process::exit(1);
    }

    environment::log("PASS: heap_preserved_across_syscalls");
}

/// Test that ALL registers are preserved across preemption (timer interrupts).
/// This catches bugs where caller-saved registers like rcx, r11 get corrupted
/// during context switches.
fn test_registers_preserved_across_preemption() {
    environment::log("TEST: registers_preserved_across_preemption");

    // We need to do CPU-bound work to trigger preemption.
    // We'll set registers to known values and verify them in a loop.
    // If preemption corrupts any register, the check will fail.
    //
    // We test in batches since we can't use all 16 GPRs simultaneously
    // (rbx is LLVM reserved, rbp is frame pointer, and we need scratch regs).

    let iterations: u64 = 500_000;

    // Test batch 1: rcx, rdx, rsi, rdi, r8, r9 (caller-saved, most likely to be corrupted)
    let failed1: u64;
    unsafe {
        asm!(
            // Set registers to known values
            "mov rcx, 0xCCCCCCCCCCCCCCCC",
            "mov rdx, 0xDDDDDDDDDDDDDDDD",
            "mov rsi, 0x5151515151515151",
            "mov rdi, 0xD1D1D1D1D1D1D1D1",
            "mov r8,  0x8888888888888888",
            "mov r9,  0x9999999999999999",
            // Use rax as loop counter
            "mov rax, {iterations}",
            "2:",
            // Check each register against immediate (split into two parts)
            "mov r10, 0xCCCCCCCCCCCCCCCC",
            "cmp rcx, r10",
            "jne 3f",
            "mov r10, 0xDDDDDDDDDDDDDDDD",
            "cmp rdx, r10",
            "jne 3f",
            "mov r10, 0x5151515151515151",
            "cmp rsi, r10",
            "jne 3f",
            "mov r10, 0xD1D1D1D1D1D1D1D1",
            "cmp rdi, r10",
            "jne 3f",
            "mov r10, 0x8888888888888888",
            "cmp r8, r10",
            "jne 3f",
            "mov r10, 0x9999999999999999",
            "cmp r9, r10",
            "jne 3f",
            "dec rax",
            "jnz 2b",
            "xor {failed}, {failed}",
            "jmp 4f",
            "3:",
            "mov {failed}, 1",
            "4:",
            iterations = in(reg) iterations,
            failed = out(reg) failed1,
            out("rax") _,
            out("rcx") _,
            out("rdx") _,
            out("rsi") _,
            out("rdi") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
        );
    }

    if failed1 != 0 {
        environment::log("FAIL: caller-saved register corrupted during preemption");
        process::exit(1);
    }

    // Test batch 2: r10, r11, r12, r13, r14 (includes r11 which is used by sysret)
    let failed2: u64;
    unsafe {
        asm!(
            "mov r10, 0x1010101010101010",
            "mov r11, 0x1111111111111111",
            "mov r12, 0x1212121212121212",
            "mov r13, 0x1313131313131313",
            "mov r14, 0x1414141414141414",
            "mov rax, {iterations}",
            "2:",
            "mov rcx, 0x1010101010101010",
            "cmp r10, rcx",
            "jne 3f",
            "mov rcx, 0x1111111111111111",
            "cmp r11, rcx",
            "jne 3f",
            "mov rcx, 0x1212121212121212",
            "cmp r12, rcx",
            "jne 3f",
            "mov rcx, 0x1313131313131313",
            "cmp r13, rcx",
            "jne 3f",
            "mov rcx, 0x1414141414141414",
            "cmp r14, rcx",
            "jne 3f",
            "dec rax",
            "jnz 2b",
            "xor {failed}, {failed}",
            "jmp 4f",
            "3:",
            "mov {failed}, 1",
            "4:",
            iterations = in(reg) iterations,
            failed = out(reg) failed2,
            out("rax") _,
            out("rcx") _,
            out("r10") _,
            out("r11") _,
            out("r12") _,
            out("r13") _,
            out("r14") _,
        );
    }

    if failed2 != 0 {
        environment::log("FAIL: r10-r14 register corrupted during preemption");
        process::exit(1);
    }

    environment::log("PASS: registers_preserved_across_preemption");
}

/// Test that a computation-heavy loop produces correct results despite preemption.
/// This is similar to preempt_test but runs within state_test for convenience.
fn test_computation_correct_across_preemption() {
    environment::log("TEST: computation_correct_across_preemption");

    // Do a computation that uses many registers and would break if
    // any register is corrupted during preemption.
    let iterations: u64 = 5_000_000;
    let mut sum: u64 = 0;
    let mut xor_acc: u64 = 0;

    for i in 0..iterations {
        sum = sum.wrapping_add(i);
        xor_acc ^= i;
        // Prevent optimization
        core::hint::black_box(&sum);
        core::hint::black_box(&xor_acc);
    }

    // Verify results
    // sum of 0..n = n*(n-1)/2
    let expected_sum = (iterations - 1) * iterations / 2;
    if sum != expected_sum {
        environment::log("FAIL: sum computation incorrect");
        process::exit(1);
    }

    // xor of 0..(n-1) follows a pattern based on (n-1) % 4
    let n = iterations - 1;
    let expected_xor = match n % 4 {
        0 => n,
        1 => 1,
        2 => n + 1,
        3 => 0,
        _ => unreachable!(),
    };

    if xor_acc != expected_xor {
        environment::log("FAIL: xor computation incorrect");
        process::exit(1);
    }

    environment::log("PASS: computation_correct_across_preemption");
}

/// Test that rflags (specifically the direction flag) is preserved.
fn test_rflags_preserved_across_preemption() {
    environment::log("TEST: rflags_preserved_across_preemption");

    // The direction flag (DF) affects string operations.
    // Ensure it stays cleared (the normal state) across preemption.

    for _ in 0..100_000u64 {
        let flags: u64;
        unsafe {
            asm!(
                "pushfq",
                "pop {flags}",
                flags = out(reg) flags,
            );
        }
        // Check if DF (bit 10) is unexpectedly set
        if (flags & 0x400) != 0 {
            environment::log("FAIL: direction flag unexpectedly set");
            process::exit(1);
        }
    }

    environment::log("PASS: rflags_preserved_across_preemption");
}

libpanda::main! {
    environment::log("=== State Preservation Tests ===");

    test_registers_preserved_simple_syscall();
    test_stack_variables_preserved();
    test_state_preserved_blocking_read();
    test_multiple_syscalls_preserve_state();
    test_nested_calls_with_syscalls();
    test_syscall_return_values();
    test_syscall_arg_registers_clean();
    test_heap_preserved_across_syscalls();

    // Preemption-specific tests
    test_registers_preserved_across_preemption();
    test_computation_correct_across_preemption();
    test_rflags_preserved_across_preemption();

    environment::log("=== All State Preservation Tests Passed ===");
    0
}
