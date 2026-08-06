//! VT-d hardware register interface for one remapping unit.
//!
//! Generic over [`MmioAccess`] so it can be tested against a fake register
//! backing and used against real MMIO in the kernel.

use crate::hal::MmioAccess;
use crate::types::IommuError;

const REG_VERSION: usize = 0x000;
const REG_CAP: usize = 0x008;
const REG_ECAP: usize = 0x010;
const REG_GCMD: usize = 0x018;
const REG_GSTS: usize = 0x01C;
const REG_RTADDR: usize = 0x020;
const REG_CCMD: usize = 0x028;

/// GCMD: enable translation.
const GCMD_TE: u32 = 1 << 31;
/// GCMD: set root table pointer.
const GCMD_SRTP: u32 = 1 << 30;

/// GSTS: translation enable status.
const GSTS_TES: u32 = 1 << 31;
/// GSTS: root table pointer status.
const GSTS_RTPS: u32 = 1 << 30;

/// CCMD: invalidate context cache (global invalidation), plus "invalidate" bit (top bit of hi dword).
const CCMD_ICC: u64 = 1 << 63;
/// CCMD: global invalidation granularity (CIRG = 01).
const CCMD_CIRG_GLOBAL: u64 = 1 << 61;

/// IOTLB: invalidate, global granularity, and "drain" bits set on the high dword
/// of the (offset+8) register.
const IOTLB_IVT: u64 = 1 << 63;
const IOTLB_IIRG_GLOBAL: u64 = 1 << 60;

/// Maximum time to spend polling a status register before giving up, bounded
/// by TSC cycles rather than a loop-iteration count. An iteration count is
/// the wrong unit here: each iteration is an MMIO read, and MMIO reads are
/// trapped VM-exits under emulation (QEMU TCG) costing microseconds instead
/// of nanoseconds, so a fixed iteration count silently assumes a cost per
/// iteration that varies wildly between real hardware, KVM, and TCG. The TSC
/// advances in real time regardless of trap overhead, so bounding by elapsed
/// cycles gives a bound in actual wall-clock time on any backend. Status
/// registers ack within microseconds on both real hardware and QEMU; one
/// second of headroom is generous.
const POLL_TIMEOUT_CYCLES: u64 = 1_000_000_000;

#[inline]
fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Typed access to a single VT-d remapping unit's memory-mapped register bank.
pub struct VtdRegisters<M: MmioAccess> {
    mmio: M,
    /// `CAP.IRO * 16`: byte offset of the IOTLB invalidation register block.
    iotlb_offset: usize,
    /// Whether `ECAP.QI` (queued invalidation) is supported by this unit.
    qi_capable: bool,
}

impl<M: MmioAccess> VtdRegisters<M> {
    /// Construct register access over `mmio`, reading CAP/ECAP to discover the
    /// IOTLB register offset and queued-invalidation support up front (risk #3).
    pub fn new(mmio: M) -> Self {
        // Safety: caller-provided `MmioAccess` guarantees the region covers at
        // least the fixed register block used here.
        let cap = unsafe { mmio.read_u64(REG_CAP) };
        let ecap = unsafe { mmio.read_u64(REG_ECAP) };

        let iro = (cap >> 8) & 0x3ff;
        let iotlb_offset = (iro * 16) as usize;
        let qi_capable = (ecap & (1 << 1)) != 0;

        Self {
            mmio,
            iotlb_offset,
            qi_capable,
        }
    }

    pub fn read_version(&self) -> u32 {
        unsafe { self.mmio.read_u32(REG_VERSION) }
    }

    pub fn read_cap(&self) -> u64 {
        unsafe { self.mmio.read_u64(REG_CAP) }
    }

    pub fn read_ecap(&self) -> u64 {
        unsafe { self.mmio.read_u64(REG_ECAP) }
    }

    pub fn qi_capable(&self) -> bool {
        self.qi_capable
    }

    /// Set the root table pointer and wait for hardware to acknowledge it.
    pub fn set_root_table(&self, phys: u64) -> Result<(), IommuError> {
        unsafe {
            self.mmio.write_u64(REG_RTADDR, phys);
            let gcmd = self.mmio.read_u32(REG_GCMD);
            self.mmio.write_u32(REG_GCMD, gcmd | GCMD_SRTP);
        }
        self.poll_gsts(GSTS_RTPS, GSTS_RTPS)
    }

    /// Enable translation and wait for hardware to acknowledge it.
    ///
    /// Risk #2: some implementations require queued invalidation to be set up
    /// before this succeeds even with register-based invalidation elsewhere.
    /// If `qi_capable()` is true and this hangs, queued invalidation setup is
    /// the first thing to check.
    pub fn enable_translation(&self) -> Result<(), IommuError> {
        unsafe {
            let gcmd = self.mmio.read_u32(REG_GCMD);
            self.mmio.write_u32(REG_GCMD, gcmd | GCMD_TE);
        }
        self.poll_gsts(GSTS_TES, GSTS_TES)
    }

    pub fn translation_enabled(&self) -> bool {
        (unsafe { self.mmio.read_u32(REG_GSTS) } & GSTS_TES) != 0
    }

    /// Global context-cache invalidation.
    pub fn invalidate_context_cache(&self) -> Result<(), IommuError> {
        unsafe {
            self.mmio
                .write_u64(REG_CCMD, CCMD_ICC | CCMD_CIRG_GLOBAL);
        }
        self.poll_ccmd_done()
    }

    /// Global IOTLB invalidation. Offset computed from `CAP.IRO` at construction
    /// time (risk #3): the IOTLB registers are not at a fixed offset.
    pub fn invalidate_iotlb_global(&self) -> Result<(), IommuError> {
        // The invalidate command is written to the second qword of the IOTLB
        // register pair, at `iotlb_offset + 8`.
        unsafe {
            self.mmio
                .write_u64(self.iotlb_offset + 8, IOTLB_IVT | IOTLB_IIRG_GLOBAL);
        }
        self.poll_iotlb_done()
    }

    fn poll_gsts(&self, mask: u32, expect: u32) -> Result<(), IommuError> {
        let deadline = rdtsc().wrapping_add(POLL_TIMEOUT_CYCLES);
        loop {
            let gsts = unsafe { self.mmio.read_u32(REG_GSTS) };
            if gsts & mask == expect {
                return Ok(());
            }
            if rdtsc() >= deadline {
                return Err(IommuError::HardwareError);
            }
            core::hint::spin_loop();
        }
    }

    fn poll_ccmd_done(&self) -> Result<(), IommuError> {
        let deadline = rdtsc().wrapping_add(POLL_TIMEOUT_CYCLES);
        loop {
            let ccmd = unsafe { self.mmio.read_u64(REG_CCMD) };
            if ccmd & CCMD_ICC == 0 {
                return Ok(());
            }
            if rdtsc() >= deadline {
                return Err(IommuError::HardwareError);
            }
            core::hint::spin_loop();
        }
    }

    fn poll_iotlb_done(&self) -> Result<(), IommuError> {
        let deadline = rdtsc().wrapping_add(POLL_TIMEOUT_CYCLES);
        loop {
            let iotlb = unsafe { self.mmio.read_u64(self.iotlb_offset + 8) };
            if iotlb & IOTLB_IVT == 0 {
                return Ok(());
            }
            if rdtsc() >= deadline {
                return Err(IommuError::HardwareError);
            }
            core::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spinning_top::Spinlock;

    /// A fake register bank backed by a byte-addressed store, for host testing.
    struct FakeMmio {
        regs: Spinlock<alloc::collections::BTreeMap<usize, u64>>,
    }

    impl FakeMmio {
        fn new() -> Self {
            Self {
                regs: Spinlock::new(alloc::collections::BTreeMap::new()),
            }
        }

        fn set(&self, offset: usize, val: u64) {
            self.regs.lock().insert(offset, val);
        }
    }

    unsafe impl MmioAccess for FakeMmio {
        unsafe fn read_u32(&self, offset: usize) -> u32 {
            *self.regs.lock().get(&offset).unwrap_or(&0) as u32
        }
        unsafe fn write_u32(&self, offset: usize, val: u32) {
            // Writing GCMD immediately flips the matching GSTS bit, simulating
            // hardware that acknowledges instantly (good enough for unit tests
            // that don't exercise real timing).
            self.regs.lock().insert(offset, val as u64);
            if offset == REG_GCMD {
                self.regs.lock().insert(REG_GSTS, val as u64);
            }
        }
        unsafe fn read_u64(&self, offset: usize) -> u64 {
            *self.regs.lock().get(&offset).unwrap_or(&0)
        }
        unsafe fn write_u64(&self, offset: usize, val: u64) {
            self.regs.lock().insert(offset, val);
            // Simulate instant completion for CCMD/IOTLB invalidation.
            if offset == REG_CCMD {
                self.regs.lock().insert(offset, val & !CCMD_ICC);
            }
        }
    }

    #[test]
    fn iotlb_offset_computed_from_cap_iro() {
        let mmio = FakeMmio::new();
        // IRO = 0x20 (bits 8..18 of CAP) -> offset = 0x20 * 16 = 0x200
        mmio.set(REG_CAP, 0x20 << 8);
        let regs = VtdRegisters::new(mmio);
        assert_eq!(regs.iotlb_offset, 0x200);
    }

    #[test]
    fn qi_capable_read_from_ecap() {
        let mmio = FakeMmio::new();
        mmio.set(REG_ECAP, 1 << 1);
        let regs = VtdRegisters::new(mmio);
        assert!(regs.qi_capable());

        let mmio2 = FakeMmio::new();
        let regs2 = VtdRegisters::new(mmio2);
        assert!(!regs2.qi_capable());
    }

    #[test]
    fn enable_translation_succeeds_when_hardware_acks() {
        let mmio = FakeMmio::new();
        let regs = VtdRegisters::new(mmio);
        assert!(regs.enable_translation().is_ok());
        assert!(regs.translation_enabled());
    }
}
