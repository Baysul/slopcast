fn main() {
    println!("cargo:rerun-if-changed=resources/windows.rc");
    println!("cargo:rerun-if-changed=resources/windows.manifest");
    println!("cargo:rerun-if-changed=resources/icons/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("resources/windows.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap_or_else(|error| panic!("Cannot embed desktop resources: {error}"));
    }
}
