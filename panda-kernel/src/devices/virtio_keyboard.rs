//! Virtio keyboard driver with blocking read support.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use spinning_top::{RwSpinlock, Spinlock};
use virtio_drivers::{
    device::input::VirtIOInput,
    transport::pci::{PciTransport, bus::PciRoot},
};

use x86_64::structures::idt::InterruptStackFrame;

use crate::{
    apic,
    device_address::DeviceAddress,
    interrupts::{self, IrqHandlerFunc},
    ioapic,
    pci::device::PciDevice,
    waker::Waker,
};

use super::virtio_hal::VirtioHal;

/// Input event from virtio-input device (matches Linux input_event structure)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InputEvent {
    /// Event type (EV_KEY, EV_REL, etc.)
    pub event_type: u16,
    /// Event code (KEY_A, KEY_ENTER, etc.)
    pub code: u16,
    /// Event value (0=release, 1=press, 2=repeat)
    pub value: u32,
}

/// Event types from Linux input.h
pub const EV_KEY: u16 = 0x01;

/// Ring buffer for keyboard events
struct RingBuffer<const N: usize> {
    buffer: [Option<InputEvent>; N],
    read_pos: usize,
    write_pos: usize,
}

impl<const N: usize> RingBuffer<N> {
    const fn new() -> Self {
        Self {
            buffer: [None; N],
            read_pos: 0,
            write_pos: 0,
        }
    }

    fn push(&mut self, event: InputEvent) {
        self.buffer[self.write_pos] = Some(event);
        self.write_pos = (self.write_pos + 1) % N;
        // If buffer is full, advance read position (drop oldest)
        if self.write_pos == self.read_pos {
            self.read_pos = (self.read_pos + 1) % N;
        }
    }

    fn pop(&mut self) -> Option<InputEvent> {
        if self.read_pos == self.write_pos {
            // Check if there's actually data or if buffer is empty
            if self.buffer[self.read_pos].is_none() {
                return None;
            }
        }
        let event = self.buffer[self.read_pos].take();
        if event.is_some() {
            self.read_pos = (self.read_pos + 1) % N;
        }
        event
    }

    fn is_empty(&self) -> bool {
        self.read_pos == self.write_pos && self.buffer[self.read_pos].is_none()
    }
}

/// A virtio keyboard device
pub struct VirtioKeyboard {
    device: VirtIOInput<VirtioHal, PciTransport>,
    buffer: RingBuffer<64>,
    waker: Arc<Waker>,
    address: DeviceAddress,
}

impl VirtioKeyboard {
    /// Pop an event from the buffer
    pub fn pop_event(&mut self) -> Option<InputEvent> {
        self.buffer.pop()
    }

    /// Check if the buffer has events
    pub fn has_events(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Get the waker for this keyboard
    pub fn waker(&self) -> Arc<Waker> {
        self.waker.clone()
    }

    /// Get the device address
    pub fn address(&self) -> &DeviceAddress {
        &self.address
    }

    /// Poll the device for new events (called from IRQ handler)
    pub fn poll(&mut self) {
        let mut count = 0;
        // Poll virtio device and push events to buffer
        while let Some(event) = self.device.pop_pending_event() {
            count += 1;
            // Only care about key events
            if event.event_type == EV_KEY {

                self.buffer.push(InputEvent {
                    event_type: event.event_type,
                    code: event.code,
                    value: event.value,
                });
            }
        }


        // Acknowledge the interrupt after consuming events
        self.device.ack_interrupt();

        // Wake any waiting process if we have events
        if self.has_events() {
            self.waker.wake();
        }
    }
}

/// Registry of keyboards by device address
static KEYBOARDS: RwSpinlock<BTreeMap<DeviceAddress, Arc<Spinlock<VirtioKeyboard>>>> =
    RwSpinlock::new(BTreeMap::new());

/// Get a keyboard by its device address
pub fn get_keyboard(address: &DeviceAddress) -> Option<Arc<Spinlock<VirtioKeyboard>>> {
    KEYBOARDS.read().get(address).cloned()
}

/// IRQ handler for virtio keyboard interrupts
extern "x86-interrupt" fn keyboard_irq_handler(_stack_frame: InterruptStackFrame) {
    // Poll all keyboards for new events
    poll_all();
    // Send end-of-interrupt
    apic::eoi();
}

/// Initialize a virtio keyboard from a PCI device
pub fn init_from_pci_device(pci_device: PciDevice) {
    use log::debug;

    let pci_address = pci_device.address();
    let address = DeviceAddress::Pci {
        bus: pci_address.bus,
        device: pci_address.slot,
        function: pci_address.function,
    };

    // Get interrupt info before we consume the device
    let irq_line = pci_device.interrupt_line();
    let irq_pin = pci_device.interrupt_pin();

    debug!(
        "Initializing virtio keyboard at {} (IRQ line={}, pin={})",
        address, irq_line, irq_pin
    );

    let mut root = PciRoot::new(pci_device.clone());
    let device_function = pci_address.into();
    let transport = PciTransport::new::<VirtioHal, PciDevice>(&mut root, device_function)
        .expect("Could not create PCI transport for virtio keyboard");

    let device = VirtIOInput::<VirtioHal, PciTransport>::new(transport)
        .expect("Could not initialize virtio keyboard");

    let keyboard = VirtioKeyboard {
        device,
        buffer: RingBuffer::new(),
        waker: Waker::new(),
        address: address.clone(),
    };

    let keyboard = Arc::new(Spinlock::new(keyboard));

    // Register in global map
    KEYBOARDS.write().insert(address, keyboard);

    // Set up IRQ handler if the device has an interrupt configured
    if irq_pin != 0 && irq_line != 0 && irq_line != 0xFF {
        debug!("Registering keyboard IRQ handler for IRQ {}", irq_line);
        // Register the handler in the IDT
        interrupts::set_irq_handler(irq_line, Some(keyboard_irq_handler as IrqHandlerFunc));
        // Configure the IOAPIC to route this IRQ to the correct vector
        let vector = 0x20 + irq_line;
        ioapic::configure_irq(irq_line, vector);
    }

    debug!("Virtio keyboard initialized");
}

/// Poll all keyboards for new events (called from IRQ handler)
pub fn poll_all() {
    let keyboards = KEYBOARDS.read();
    for keyboard in keyboards.values() {
        keyboard.lock().poll();
    }
}
