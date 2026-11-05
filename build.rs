fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // Gives the program its own icon in Explorer and on the taskbar.
    if std::path::Path::new("assets/icon.ico").exists() {
        let _ = embed_resource::compile("assets/app.rc", embed_resource::NONE);
    }
}
