fn main() {
    println!("cargo:rerun-if-changed=../target/release/driverkeep.exe");
    let _ = embed_resource::compile("../assets/app.rc", embed_resource::NONE);
}
