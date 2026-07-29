fn main() {
    napi_build::setup();
    println!("cargo:rustc-link-lib=dylib=X11");
}
