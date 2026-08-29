fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
    }

    // Windows: embed the app icon and version metadata into the .exe. Without
    // it the binary shows the generic blank icon everywhere the shell displays
    // it — and the Start Menu shortcut inherits that too.
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=wix/whisper-push.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("wix/whisper-push.ico");
        res.set("ProductName", "Whisper Push");
        res.set(
            "FileDescription",
            "Whisper Push — push-to-talk voice dictation",
        );
        res.set("CompanyName", "Whisper Push");
        res.set("LegalCopyright", "MIT licensed");
        if let Err(e) = res.compile() {
            println!("cargo:warning=couldn't embed the Windows icon: {e}");
        }
    }
}
