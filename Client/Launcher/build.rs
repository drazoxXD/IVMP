fn main() {
    // Exe and window icon from the repo's Icons folder (resource id 1, read by the UI).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=../../Icons/logo.ico");
        winresource::WindowsResource::new()
            .set_icon_with_id("../../Icons/logo.ico", "1")
            .compile()
            .expect("failed to embed the launcher icon");
    }

    // Embed a manifest so the Win32 controls get modern visual styles (comctl32 v6).
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:level='asInvoker'");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' \
             name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
             processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
}
