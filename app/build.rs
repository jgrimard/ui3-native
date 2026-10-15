//! Embeds the application icon and version information into ui3-native.exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "UI3 Native");
        res.set("FileDescription", "UI3 Native - Blue Iris client");
        res.set("LegalCopyright", "UI3 Native contributors; based on UI3 by bp2008");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
