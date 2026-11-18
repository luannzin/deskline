//! Windows: puts the icon and version info into deskline.exe, for Explorer, the taskbar, Task
//! Manager and the UAC prompt. Needs rc.exe (Windows SDK) on MSVC, or mingw-w64's windres when
//! cross-building from Linux.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // the target, not the machine building it
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "Deskline").set("FileDescription", "Deskline").set_icon("assets/icon.ico");
    res.compile().unwrap_or_else(|e| panic!("couldn't put the icon and version info into deskline.exe: {e}"));
}
