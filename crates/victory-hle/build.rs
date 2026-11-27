fn main() {
    // Tests of title memory need the low 4 GiB (see apps/victory/build.rs).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-pagezero_size,0x10000");
    }
}
