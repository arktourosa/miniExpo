// Embeds assets/app.ico into the .exe as icon resource #1.
// GPUI loads resource #1 for the window / taskbar icon on Windows.
fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "miniExpo");
        res.set("FileDescription", "miniExpo");
        if let Err(e) = res.compile() {
            // Don't fail the build just because rc.exe wasn't found.
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
