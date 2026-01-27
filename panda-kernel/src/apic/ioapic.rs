//! I/O APIC driver for external interrupt routing.
//!
//! The IOAPIC routes external interrupts (from PCI devices, etc.) to the
//! Local APIC. Each IOAPIC has 24 redirection entries that map IRQ lines
//! to interrupt vectors.

use acpi::sdt::madt::{Madt, MadtEntry};
use log::debug;
use spinning_top::Spinlock;
use x86_64::PhysAddr;

use crate::memory::PhysicalMapping;

/// IOAPIC register offsets (accessed via index/data registers)
mod reg {
    #![allow(dead_code)]
    pub const ID: u32 = 0x00;
    pub const VERSION: u32 = 0x01;
    pub const ARBITRATION: u32 = 0x02;
    /// Redirection table entries start at 0x10, each entry is 2 32-bit registers
    pub const REDIRECTION_BASE: u32 = 0x10;
}

/// Delivery mode for interrupts
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum DeliveryMode {
    Fixed = 0b000,
    LowestPriority = 0b001,
    Smi = 0b010,
    Nmi = 0b100,
    Init = 0b101,
    ExtInt = 0b111,
}

/// IOAPIC redirection entry
#[derive(Debug, Clone, Copy)]
pub struct RedirectionEntry {
    pub vector: u8,
    pub delivery_mode: DeliveryMode,
    pub destination_mode_logical: bool,
    pub polarity_low: bool,
    pub trigger_level: bool,
    pub masked: bool,
    pub destination: u8,
}

impl RedirectionEntry {
    /// Create a new redirection entry with default settings
    pub fn new(vector: u8, destination: u8) -> Self {
        Self {
            vector,
            delivery_mode: DeliveryMode::Fixed,
            destination_mode_logical: false,
            polarity_low: false,
            trigger_level: false,
            masked: false,
            destination,
        }
    }

    /// Convert to the 64-bit register format
    fn to_raw(&self) -> u64 {
        let mut value: u64 = self.vector as u64;
        value |= (self.delivery_mode as u64) << 8;
        if self.destination_mode_logical {
            value |= 1 << 11;
        }
        if self.polarity_low {
            value |= 1 << 13;
        }
        if self.trigger_level {
            value |= 1 << 15;
        }
        if self.masked {
            value |= 1 << 16;
        }
        value |= (self.destination as u64) << 56;
        value
    }
}

/// I/O APIC instance
struct IoApic {
    /// MMIO mapping for IOAPIC registers (kept alive for kernel lifetime).
    mapping: PhysicalMapping,
    /// Global System Interrupt base for this IOAPIC (for future multi-IOAPIC support)
    _gsi_base: u32,
    /// Number of redirection entries
    max_entries: u8,
}

impl IoApic {
    /// Create a new IOAPIC at the given physical address
    fn new(base_phys: u64, gsi_base: u32) -> Self {
        // Map IOAPIC registers (index at 0x00, data at 0x10)
        let mapping = PhysicalMapping::new(PhysAddr::new(base_phys), 0x20);

        let mut ioapic = Self {
            mapping,
            _gsi_base: gsi_base,
            max_entries: 0,
        };

        // Read version register to get max entries
        let version = ioapic.read(reg::VERSION);
        ioapic.max_entries = ((version >> 16) & 0xFF) as u8 + 1;

        ioapic
    }

    /// Write to the IOAPIC index register
    #[inline]
    fn write_index(&self, index: u32) {
        self.mapping.write(0, index);
    }

    /// Read from the IOAPIC data register
    #[inline]
    fn read_data(&self) -> u32 {
        self.mapping.read(0x10)
    }

    /// Write to the IOAPIC data register
    #[inline]
    fn write_data(&self, value: u32) {
        self.mapping.write(0x10, value);
    }

    /// Read a 32-bit register
    fn read(&self, reg: u32) -> u32 {
        self.write_index(reg);
        self.read_data()
    }

    /// Write a 32-bit register
    fn write(&self, reg: u32, value: u32) {
        self.write_index(reg);
        self.write_data(value);
    }

    /// Get the IOAPIC ID
    #[allow(dead_code)]
    fn id(&self) -> u8 {
        ((self.read(reg::ID) >> 24) & 0xF) as u8
    }

    /// Set a redirection entry for an IRQ
    fn set_redirection(&self, irq: u8, entry: RedirectionEntry) {
        let reg_base = reg::REDIRECTION_BASE + (irq as u32 * 2);
        let raw = entry.to_raw();

        // Write low 32 bits first (with masked bit set to avoid spurious interrupts)
        self.write(reg_base, (raw as u32) | (1 << 16));
        // Write high 32 bits
        self.write(reg_base + 1, (raw >> 32) as u32);
        // Write low 32 bits again with correct mask state
        self.write(reg_base, raw as u32);
    }

    /// Mask (disable) an IRQ
    fn mask_irq(&self, irq: u8) {
        let reg_base = reg::REDIRECTION_BASE + (irq as u32 * 2);
        let low = self.read(reg_base);
        self.write(reg_base, low | (1 << 16));
    }

    /// Unmask (enable) an IRQ
    fn unmask_irq(&self, irq: u8) {
        let reg_base = reg::REDIRECTION_BASE + (irq as u32 * 2);
        let low = self.read(reg_base);
        self.write(reg_base, low & !(1 << 16));
    }
}

/// Global IOAPIC instance (we only support one IOAPIC for now)
static IOAPIC: Spinlock<Option<IoApic>> = Spinlock::new(None);

/// Initialize the IOAPIC from ACPI MADT
pub fn init() {
    crate::acpi::with_table::<Madt>(|madt| {
        let madt = madt.expect("No MADT found");

        for entry in madt.entries() {
            if let MadtEntry::IoApic(ioapic_entry) = entry {
                // Copy fields from packed struct to avoid unaligned access
                let address = ioapic_entry.io_apic_address;
                let gsi_base = ioapic_entry.global_system_interrupt_base;
                let ioapic = IoApic::new(address as u64, gsi_base);

                debug!(
                    "IOAPIC: base={:#x}, GSI base={}, max entries={}",
                    address, gsi_base, ioapic.max_entries
                );

                *IOAPIC.lock() = Some(ioapic);

                // Only handle the first IOAPIC for now
                break;
            }
        }
    });
}

/// Execute a function with the IOAPIC.
fn with_ioapic<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&IoApic) -> R,
{
    let guard = IOAPIC.lock();
    guard.as_ref().map(f)
}

/// Configure an IRQ to route to a specific vector on the BSP (CPU 0)
pub fn configure_irq(irq: u8, vector: u8) {
    with_ioapic(|ioapic| {
        if irq >= ioapic.max_entries {
            return; // IRQ out of range
        }

        // Route to CPU 0 (APIC ID 0)
        let entry = RedirectionEntry::new(vector, 0);
        ioapic.set_redirection(irq, entry);

        debug!("IOAPIC: Configured IRQ {} -> vector {:#x}", irq, vector);
    });
}

/// Configure a PCI IRQ with edge-triggered, active-low settings.
/// Note: PCI INTx spec says level-triggered, active-low, but QEMU's
/// emulated virtio-pci works better with edge-triggered to avoid
/// interrupt storms when we can't immediately consume the used ring.
pub fn configure_pci_irq(irq: u8, vector: u8) {
    with_ioapic(|ioapic| {
        if irq >= ioapic.max_entries {
            return; // IRQ out of range
        }

        // Route to CPU 0 (APIC ID 0) with edge-triggered settings
        // Using active-high because QEMU's virtio-pci seems to use positive edges
        let entry = RedirectionEntry {
            vector,
            delivery_mode: DeliveryMode::Fixed,
            destination_mode_logical: false,
            polarity_low: false,  // Active-high for QEMU virtio
            trigger_level: false, // Edge-triggered
            masked: false,
            destination: 0,
        };
        ioapic.set_redirection(irq, entry);

        debug!(
            "IOAPIC: Configured PCI IRQ {} -> vector {:#x} (edge-triggered, active-high)",
            irq, vector
        );
    });
}

/// Mask (disable) an IRQ in the IOAPIC
pub fn mask_irq(irq: u8) {
    with_ioapic(|ioapic| {
        if irq < ioapic.max_entries {
            ioapic.mask_irq(irq);
        }
    });
}

/// Unmask (enable) an IRQ in the IOAPIC
pub fn unmask_irq(irq: u8) {
    with_ioapic(|ioapic| {
        if irq < ioapic.max_entries {
            ioapic.unmask_irq(irq);
        }
    });
}
