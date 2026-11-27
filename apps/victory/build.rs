fn main() {
    // The title's 32-bit heap lies below 4 GiB, which a macOS program gives
    // whole to __PAGEZERO unless told otherwise.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-pagezero_size,0x10000");
    }
}
