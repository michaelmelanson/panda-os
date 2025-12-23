mod handler;

use core::{num::NonZero, pin::Pin};

use acpi::AcpiTables;
use spinning_top::RwSpinlock;
use x86_64::PhysAddr;

use handler::AcpiHandler;

static ACPI_TABLES: RwSpinlock<Option<AcpiTables<AcpiHandler>>> = RwSpinlock::new(None);

pub fn init<'a>(acpi2_rsdp: PhysAddr) {
    let acpi2_rsdp =
        NonZero::new(acpi2_rsdp.as_u64() as usize).expect("ACPI RSDP2 must be non-zero");

    let mut acpi_tables = ACPI_TABLES.write();
    unsafe {
        *acpi_tables = Some(
            ::acpi::AcpiTables::from_rsdp(AcpiHandler, acpi2_rsdp.get())
                .expect("Could not get ACPI tables"),
        );
    };
}

fn with_acpi_tables(f: impl Fn(&AcpiTables<AcpiHandler>)) {
    let acpi_tables = ACPI_TABLES.read();
    let acpi_tables = acpi_tables.as_ref().expect("ACPI not initialized");
    f(acpi_tables)
}

pub fn with_table<T: acpi::AcpiTable>(f: impl Fn(Option<Pin<&T>>)) {
    with_acpi_tables(|acpi_tables| {
        if let Some(table) = acpi_tables.find_table::<T>() {
            f(Some(table.get()))
        } else {
            f(None)
        }
    })
}
