//! APIC drivers for interrupt management.
//!
//! This module contains drivers for both the Local APIC and I/O APIC:
//! - Local APIC: Timer interrupts, inter-processor interrupts
//! - I/O APIC: External interrupt routing from PCI devices

pub mod ioapic;
mod timer;

pub use timer::{calibrate_timer, set_timer_oneshot, stop_timer, ticks_per_ms};

use core::sync::atomic::{AtomicU64, Ordering};

use log::debug;
use spinning_top::Spinlock;
use x86_64::{PhysAddr, VirtAddr};

use crate::memory::PhysicalMapping;

/// Local APIC register offsets
#[allow(dead_code)]
mod reg {
    pub const ID: u32 = 0x020;
    pub const VERSION: u32 = 0x030;
    pub const TPR: u32 = 0x080; // Task Priority Register
    pub const EOI: u32 = 0x0B0; // End of Interrupt
    pub const SPURIOUS: u32 = 0x0F0; // Spurious Interrupt Vector
    pub const ICR_LOW: u32 = 0x300; // Interrupt Command Register (low)
    pub const ICR_HIGH: u32 = 0x310; // Interrupt Command Register (high)
    pub const LVT_TIMER: u32 = 0x320; // Local Vector Table - Timer
    pub const TIMER_INITIAL: u32 = 0x380; // Timer Initial Count
    pub const TIMER_CURRENT: u32 = 0x390; // Timer Current Count
    pub const TIMER_DIVIDE: u32 = 0x3E0; // Timer Divide Configuration
}

/// Timer modes for LVT Timer register
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum TimerMode {
    OneShot = 0b00 << 17,
    Periodic = 0b01 << 17,
    TscDeadline = 0b10 << 17,
}

/// Timer divider values
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum TimerDivide {
    By1 = 0b1011,
    By2 = 0b0000,
    By4 = 0b0001,
    By8 = 0b0010,
    By16 = 0b0011,
    By32 = 0b1000,
    By64 = 0b1001,
    By128 = 0b1010,
}

/// The standard Local APIC base address (can be relocated via MSR)
const DEFAULT_APIC_BASE: u64 = 0xFEE0_0000;

/// APIC base address MSR
#[allow(dead_code)]
const IA32_APIC_BASE_MSR: u32 = 0x1B;

/// Local APIC driver
pub struct LocalApic {
    /// MMIO mapping for APIC registers (kept alive for kernel lifetime).
    mapping: PhysicalMapping,
}

impl LocalApic {
    /// Create a new Local APIC instance at the default base address.
    pub fn new() -> Self {
        let base_phys = PhysAddr::new(DEFAULT_APIC_BASE);
        // Map 4KB for APIC registers
        let mapping = PhysicalMapping::new(base_phys, 4096);
        Self { mapping }
    }

    /// Get the base virtual address of the APIC registers.
    fn base_virt(&self) -> VirtAddr {
        self.mapping.virt_addr()
    }

    /// Read a 32-bit register from the Local APIC.
    #[inline]
    pub fn read(&self, offset: u32) -> u32 {
        self.mapping.read(offset as usize)
    }

    /// Write a 32-bit value to a Local APIC register.
    #[inline]
    pub fn write(&self, offset: u32, value: u32) {
        self.mapping.write(offset as usize, value)
    }

    /// Get the Local APIC ID.
    pub fn id(&self) -> u8 {
        ((self.read(reg::ID) >> 24) & 0xFF) as u8
    }

    /// Get the Local APIC version.
    pub fn version(&self) -> u8 {
        (self.read(reg::VERSION) & 0xFF) as u8
    }

    /// Enable the Local APIC with a spurious interrupt vector.
    pub fn enable(&self, spurious_vector: u8) {
        // Set spurious vector and enable APIC (bit 8)
        let value = (spurious_vector as u32) | (1 << 8);
        self.write(reg::SPURIOUS, value);
    }

    /// Send End of Interrupt signal.
    #[inline]
    pub fn eoi(&self) {
        self.write(reg::EOI, 0);
    }

    /// Configure the timer with a specific mode, vector, and divider.
    pub fn configure_timer(&self, mode: TimerMode, vector: u8, divide: TimerDivide) {
        // Set divider
        self.write(reg::TIMER_DIVIDE, divide as u32);

        // Configure LVT Timer: vector + mode
        let lvt = (vector as u32) | (mode as u32);
        self.write(reg::LVT_TIMER, lvt);
    }

    /// Set the timer initial count (starts the timer).
    pub fn set_timer_count(&self, count: u32) {
        self.write(reg::TIMER_INITIAL, count);
    }

    /// Get the current timer count.
    pub fn timer_count(&self) -> u32 {
        self.read(reg::TIMER_CURRENT)
    }

    /// Mask (disable) the timer interrupt.
    pub fn mask_timer(&self) {
        let lvt = self.read(reg::LVT_TIMER);
        self.write(reg::LVT_TIMER, lvt | (1 << 16)); // Set mask bit
    }

    /// Unmask (enable) the timer interrupt.
    pub fn unmask_timer(&self) {
        let lvt = self.read(reg::LVT_TIMER);
        self.write(reg::LVT_TIMER, lvt & !(1 << 16)); // Clear mask bit
    }
}

/// Global Local APIC instance (for non-interrupt context operations)
static LOCAL_APIC: Spinlock<Option<LocalApic>> = Spinlock::new(None);

/// APIC base virtual address for lock-free EOI in interrupt context.
/// This is set once during init and never changes.
static APIC_BASE_VIRT: AtomicU64 = AtomicU64::new(0);

/// Initialize the Local APIC and I/O APIC.
pub fn init() {
    let apic = LocalApic::new();

    debug!("Local APIC: ID={}, version={}", apic.id(), apic.version());

    // Enable APIC with spurious vector 0xFF
    apic.enable(0xFF);

    // Store the base address for lock-free EOI
    APIC_BASE_VIRT.store(apic.base_virt().as_u64(), Ordering::Release);

    *LOCAL_APIC.lock() = Some(apic);

    // Calibrate the timer
    calibrate_timer();

    // Calibrate the TSC for nanosecond-resolution timing
    crate::time::calibrate_tsc();

    // Initialize the I/O APIC
    ioapic::init();
}

/// Execute a function with the Local APIC.
///
/// Note: Do NOT call this from interrupt handlers - use `eoi()` directly instead.
pub fn with_local_apic<F, R>(f: F) -> R
where
    F: FnOnce(&LocalApic) -> R,
{
    let guard = LOCAL_APIC.lock();
    let apic = guard.as_ref().expect("Local APIC not initialized");
    f(apic)
}

/// Send End of Interrupt to the Local APIC.
///
/// This is safe to call from interrupt handlers as it doesn't take any locks.
#[inline]
pub fn eoi() {
    let base = APIC_BASE_VIRT.load(Ordering::Acquire);
    assert!(base != 0, "Local APIC not initialized");
    unsafe {
        let eoi_ptr = (base + reg::EOI as u64) as *mut u32;
        core::ptr::write_volatile(eoi_ptr, 0);
    }
}
