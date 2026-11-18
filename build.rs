//! Windows: puts the icon and version info into deskline.exe, for Explorer, the taskbar, Task
//! Manager and the UAC prompt. Needs rc.exe (Windows SDK) on MSVC, or mingw-w64's windres when
//! cross-building from Linux.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // the target, not the machine building it
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "Deskline").set("FileDescription", "Deskline");
    let icon = "assets/icon.ico";
    if std::path::Path::new(icon).exists() {
        res.set_icon(icon);
        println!("cargo:rerun-if-changed={icon}");
    } else {
        println!("cargo:warning=no {icon} yet: deskline.exe gets the default icon");
        println!("cargo:rerun-if-changed=assets");
    }
    if let Err(e) = res.compile() {
        panic!("couldn't put the icon and version info into deskline.exe: {e}");
    }
}
