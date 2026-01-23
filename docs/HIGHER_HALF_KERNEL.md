# Higher-Half Kernel Migration Plan

## Goal

Migrate from identity-mapped kernel to higher-half kernel layout:
- Kernel in high canonical addresses (0xffff_8000_0000_0000+)
- Explicit physical memory window (replaces identity mapping)
- Explicit MMIO mapping region
- Userspace in entire lower canonical half (0x0 - 0x7fff_ffff_ffff)

## New Memory Layout

| Region | Virtual Address Range | PML4 Entries | Purpose |
|--------|----------------------|--------------|---------|
| Userspace | `0x0000_0000_0000_0000` - `0x0000_7fff_ffff_ffff` | 0-255 | Entire lower half (128 TB) |
| Physical Window | `0xffff_8000_0000_0000` - `0xffff_8fff_ffff_ffff` | 256-271 | Direct map of all physical RAM |
| MMIO Region | `0xffff_9000_0000_0000` - `0xffff_9fff_ffff_ffff` | 272-287 | Device MMIO (PCI BARs, APIC, etc.) |
| Kernel Heap | `0xffff_a000_0000_0000` - `0xffff_afff_ffff_ffff` | 288-303 | Kernel allocations |

### New Userspace Layout

| Region | Address | Notes |
|--------|---------|-------|
| ELF Code/Data | `0x0000_0000_0040_0000` | Base address for executables |
| Heap | `0x0000_0001_0000_0000` | ~1 TB, grows up |
| Buffers | `0x0000_0100_0000_0000` | 4 GB for zero-copy I/O |
| Stack | `0x0000_7fff_ff00_0000` | 16 MB, grows down |

## Kernel Relocation Strategy

The kernel is compiled as a UEFI PE/COFF application with **position-dependent code**. All code and data addresses are absolute values determined at link time. UEFI loads the kernel at an arbitrary physical address and identity-maps it.

**Solution: Runtime PE relocation**

The kernel PE binary has a `.reloc` section with base relocation data that we can use to relocate the kernel to higher-half addresses at boot time.

**How it works:**
1. Parse the `.reloc` section using `goblin` (add `pe64` feature)
2. Map kernel's physical pages to higher-half virtual addresses
3. Apply relocations: for each `DIR64` entry, add `(new_base - old_base)` to the 64-bit value
4. Jump to higher-half and remove identity mapping

**Relocation details:**
- ImageBase in PE header: `0x140000000`
- Relocation entries: ~3000 (mostly `DIR64` type for 64-bit absolute addresses)
- `.reloc` section size: ~6KB
- Relocation types: `IMAGE_REL_BASED_DIR64` (add delta to 64-bit value), `IMAGE_REL_BASED_ABSOLUTE` (skip/padding)

**Algorithm:**
```rust
fn apply_relocations(new_base: VirtAddr, image_base: u64, reloc_section: &[u8]) {
    let delta = new_base.as_u64() as i64 - image_base as i64;
    
    for block in parse_reloc_blocks(reloc_section) {
        let page_rva = block.virtual_address;
        for entry in block.entries {
            match entry.reloc_type {
                IMAGE_REL_BASED_DIR64 => {
                    let addr = new_base + page_rva + entry.offset;
                    let value = unsafe { *(addr.as_ptr::<u64>()) };
                    unsafe { *(addr.as_mut_ptr::<u64>()) = (value as i64 + delta) as u64; }
                }
                IMAGE_REL_BASED_ABSOLUTE => {} // padding, skip
                _ => panic!("unsupported relocation type"),
            }
        }
    }
}
```

## Design Principles

### RAII Wrappers for Memory Access

All physical and MMIO memory access should use RAII wrappers that:
- Ensure mappings are properly established before access
- Automatically unmap when the wrapper is dropped
- Provide type-safe access to the underlying memory

```rust
/// RAII wrapper for physical memory access
pub struct PhysicalMapping<T> {
    virt_addr: VirtAddr,
    _marker: PhantomData<T>,
}

impl<T> PhysicalMapping<T> {
    pub fn new(phys_addr: PhysAddr) -> Self { ... }
    pub fn as_ref(&self) -> &T { ... }
    pub fn as_mut(&mut self) -> &mut T { ... }
}

/// RAII wrapper for MMIO region access
pub struct MmioMapping {
    virt_addr: VirtAddr,
    size: usize,
}

impl MmioMapping {
    pub fn new(phys_addr: PhysAddr, size: usize) -> Self { ... }
    pub fn read<T>(&self, offset: usize) -> T { ... }
    pub fn write<T>(&self, offset: usize, value: T) { ... }
}

impl Drop for MmioMapping {
    fn drop(&mut self) { /* unmap region */ }
}
```

### Isolated Higher-Half Module

Page table initialization for the higher-half transition should be in its own module:
- `panda-kernel/src/memory/higher_half.rs`
- Uses low-level operations from `memory` module
- Only called during early initialization
- Clear separation from runtime memory management

## Files to Modify/Create

**New modules:**
- `panda-kernel/src/memory/higher_half.rs` - Higher-half page table setup (isolated, early-init only)
- `panda-kernel/src/memory/phys.rs` - Physical memory RAII wrapper
- `panda-kernel/src/memory/mmio.rs` - MMIO mapping RAII wrapper

**Core memory management:**
- `panda-kernel/src/memory/address.rs` - Add `phys_to_virt()` / `virt_to_phys()`
- `panda-kernel/src/memory/paging.rs` - Low-level page table ops
- `panda-kernel/src/memory/mod.rs` - Re-exports, MMIO allocator

**Subsystems to update (use new RAII wrappers):**
- `panda-kernel/src/memory/frame.rs` - Frame deallocation
- `panda-kernel/src/memory/dma.rs` - DMA buffer access
- `panda-kernel/src/devices/virtio_hal.rs` - VirtIO HAL
- `panda-kernel/src/apic/mod.rs` - Local APIC
- `panda-kernel/src/apic/ioapic.rs` - I/O APIC
- `panda-kernel/src/pci.rs` - PCIe ECAM
- `panda-kernel/src/pci/device.rs` - PCI BARs
- `panda-kernel/src/acpi/handler.rs` - ACPI tables

**Userspace layout:**
- `panda-abi/src/lib.rs` - Update address constants
- `x86_64-panda-userspace.ld` - Change base address

## Implementation Phases

### Phase 1: Infrastructure & RAII Wrappers

Add new abstractions without changing existing behavior.

1. **Create `memory/phys.rs` with `PhysicalMapping<T>` RAII wrapper:**
   - Initially uses identity mapping internally
   - Provides safe interface for physical memory access
   - Will switch to physical window later transparently

2. **Create `memory/mmio.rs` with `MmioMapping` RAII wrapper:**
   - Initially uses identity mapping internally
   - Provides volatile read/write for device registers
   - Tracks mapping for automatic cleanup

3. **Create `memory/higher_half.rs` (empty scaffold):**
   - Will contain physical window and MMIO region setup
   - Isolated from runtime memory management, only called at init

4. **Add `phys_to_virt()` / `virt_to_phys()` in `memory/address.rs`:**
   ```rust
   static PHYS_MAP_BASE: AtomicU64 = AtomicU64::new(0);  // 0 = identity mapping
   
   pub fn phys_to_virt(phys: PhysAddr) -> VirtAddr {
       let base = PHYS_MAP_BASE.load(Ordering::Relaxed);
       VirtAddr::new(base + phys.as_u64())
   }
   ```

**Verification:** All existing tests pass unchanged.

### Phase 2: Migrate Physical Memory Access to Higher Half

Move physical memory access to the physical window.

1. **In `memory/higher_half.rs`, add physical window mapper:**
   - Map all physical RAM to `0xffff_8000_0000_0000+`
   - Use 1GB/2MB huge pages for efficiency
   - Keep kernel's identity mapping intact (kernel code/data still needs it)

2. **Call from init, set `PHYS_MAP_BASE`:**
   ```rust
   higher_half::create_physical_memory_window(&memory_map);
   address::set_phys_map_base(0xffff_8000_0000_0000);
   ```

3. **Physical memory (page tables, DMA buffers, etc.) now accessed via window**

**Verification:** Kernel boots, all memory operations work via physical window.

### Phase 3: Migrate MMIO to Explicit Region

Move MMIO mappings to dedicated region.

1. **Add MMIO allocator in `memory/mmio.rs`:**
   - Bump allocator at `0xffff_9000_0000_0000`
   - `MmioMapping::new()` allocates from this region

2. **Update all MMIO users to use `MmioMapping`:**
   - APIC, IOAPIC, PCI config, PCI BARs, ACPI
   - Each gets an `MmioMapping` field instead of raw `VirtAddr`

3. **New MMIO mappings go to higher half; old identity mappings unused**

**Verification:** All device drivers work with MMIO in higher half.

### Phase 4: Relocate Kernel to Higher Half

Apply PE relocations to move kernel execution to higher-half addresses.

1. **Add `pe64` feature to goblin in `Cargo.toml`:**
   ```toml
   goblin = { version = "0.10", default-features = false, features = ["alloc", "elf32", "elf64", "pe64", "endian_fd"] }
   ```

2. **In `memory/higher_half.rs`, implement kernel relocation:**
   - Parse kernel PE headers to find `.reloc` section
   - Map kernel's physical pages to higher-half virtual addresses (e.g., `0xffff_c000_0000_0000 + offset`)
   - Apply relocations to the higher-half copy:
     - For each `IMAGE_REL_BASED_DIR64`: add delta to the 64-bit value
     - Skip `IMAGE_REL_BASED_ABSOLUTE` (padding entries)
   - Kernel is now dual-mapped: identity (original) and higher-half (relocated)

3. **Verify relocation before jumping:**
   - Read a known global variable via higher-half address
   - Compare with identity-mapped value
   - Log success

**Verification:** Kernel code/data accessible and correct at higher-half addresses.

### Phase 5: Jump to Higher Half Execution

Switch execution to the relocated kernel.

**Stack handling before jump:**
- During early boot, RSP points to UEFI-provided stack (NOT part of kernel image)
- PE relocation doesn't touch RSP - it only fixes addresses baked into code
- Must switch RSP to a higher-half address before jumping

**Static stacks** (`syscall/gdt.rs`): `SYSCALL_STACK`, `PRIVILEGE_STACK`, `INTERRUPT_STACK_*` are part of the kernel image and get mapped to higher-half automatically. We can use one of these for the transition.

1. **Calculate higher-half address of a static stack:**
   ```rust
   // SYSCALL_STACK is at identity-mapped address, calculate its higher-half equivalent
   let identity_stack_top = SYSCALL_STACK.inner.as_ptr() as u64 + SYSCALL_STACK.inner.len() as u64;
   let higher_half_stack_top = identity_stack_top - KERNEL_IDENTITY_BASE + KERNEL_HIGHER_HALF_BASE;
   ```

2. **Implement `jump_to_higher_half()` in `memory/higher_half.rs`:**
   ```rust
   pub unsafe fn jump_to_higher_half(continuation: fn() -> !) -> ! {
       // Calculate higher-half addresses
       let higher_half_stack = /* higher-half address of SYSCALL_STACK top */;
       let higher_half_continuation = /* higher-half address of continuation */;
       
       asm!(
           "mov rsp, {new_stack}",
           "jmp {continuation}",
           new_stack = in(reg) higher_half_stack,
           continuation = in(reg) higher_half_continuation,
           options(noreturn)
       );
   }
   ```

3. **In continuation function, reinitialize GDT/TSS:**
   - TSS still contains identity-mapped stack addresses for privilege transitions
   - Call `gdt::init()` again - it will now use higher-half addresses for the static stacks
   - After this, interrupts from userspace will use correctly-addressed stacks

**Verification:** Kernel continues running after jump, timer interrupts work.

### Phase 6: Remove Identity Mapping

Clean up lower-half kernel mappings.

1. **Remove kernel identity mapping:**
   - Clear PML4 entries that mapped kernel at identity addresses
   - Flush TLB

2. **Update `create_user_page_table()`:**
   - Copy only higher-half kernel entries (physical window, MMIO, kernel code/data)
   - Userspace gets entire lower half (PML4 entries 0-255)

**Verification:** Kernel runs entirely in higher half, no identity mapping.

### Phase 7: Move Userspace to Lower Half

Update userspace address layout.

1. **Update `panda-abi/src/lib.rs` constants:**
   ```rust
   pub const BUFFER_BASE: usize = 0x0000_0100_0000_0000;
   pub const STACK_BASE: usize  = 0x0000_7fff_fef0_0000;
   pub const HEAP_BASE: usize   = 0x0000_0001_0000_0000;
   ```

2. **Update `x86_64-panda-userspace.ld`:**
   ```
   . = 0x400000;
   ```

3. **Rebuild all userspace binaries**

**Verification:** All userspace test suites pass.

## Module Structure

```
panda-kernel/src/memory/
├── mod.rs              # Re-exports, init_from_uefi()
├── address.rs          # phys_to_virt(), virt_to_phys(), PHYS_MAP_BASE
├── paging.rs           # Low-level page table operations
├── higher_half.rs      # NEW: Physical window, MMIO region, kernel relocation (early-init only)
├── phys.rs             # NEW: PhysicalMapping<T> RAII wrapper
├── mmio.rs             # NEW: MmioMapping RAII wrapper, MMIO allocator
├── frame.rs            # Frame RAII guard (uses PhysicalMapping internally)
├── dma.rs              # DMA buffers (uses PhysicalMapping internally)
├── mapping.rs          # Mapping RAII for userspace regions
├── heap_allocator.rs   # Heap region selection
└── global_alloc.rs     # Global allocator wrapper
```

## Verification Plan

**IMPORTANT:** Each phase MUST include kernel tests that verify the new mappings are correct.
Tests should be added to `panda-kernel/tests/` and run via `make test`.

After each phase:
```bash
cargo build --package panda-kernel
make test
./scripts/run-qemu.sh
```

| Phase | Key Verification | Required Tests |
|-------|-----------------|----------------|
| 1 | All tests pass, no behavior change | N/A (infrastructure only) |
| 2 | Kernel boots with physical window active | Test that `physical_address_to_virtual()` returns higher-half addresses; test read/write via physical window |
| 3 | VirtIO GPU + block work with MMIO in higher half | Test MMIO region is mapped at `MMIO_REGION_BASE`; test MMIO read/write works |
| 4 | Kernel code/data correct at higher-half addresses | Test kernel globals accessible via higher-half addresses |
| 5 | Kernel runs after jump, timer interrupts work | Test execution is in higher half (check RIP); test interrupts work |
| 6 | Lower half unmapped, kernel runs entirely in higher half | Test lower-half kernel addresses are unmapped (page fault on access) |
| 7 | All userspace tests pass in lower half | Existing userspace tests verify new layout |

## Testing & Debugging

**QEMU debugging flags:**
- `qemu-system-x86_64 -d int,cpu_reset` - Log interrupts and CPU resets (useful for triple faults)
- `-s -S` - Enable GDB server, pause at start

**Relocation verification:**
- Log relocation count and compare with `objdump -p panda-kernel.efi | grep "Number of fixups"`
- Read a known global via higher-half address before jumping, compare with identity-mapped value

**Incremental testing:**
- Each phase is independently testable
- Can stop at any phase with working kernel
