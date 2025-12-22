#[panic_handler]
#[cfg(not(test))]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use log::error;

    error!("PANIC: {}", info.message());
    loop {}
}
