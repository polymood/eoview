fn main() {
    // The icon of the executable in the Windows shell. The window icon reads the same resource (ordinal 1).
    println!("cargo:rerun-if-changed=assets/eoview.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new().set_icon("assets/eoview.ico").compile().expect("icon resource");
    }
}
