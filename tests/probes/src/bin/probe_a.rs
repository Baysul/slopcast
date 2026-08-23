fn main() {
    native_rust::ensure_pipewire_init();
    match native_rust::list_audio_applications() {
        Ok(apps) => println!(
            "PROBE A: OK — pipewire init + enumeration worked, {} audio apps",
            apps.len()
        ),
        Err(e) => {
            println!("PROBE A: enumeration failed: {e}");
            std::process::exit(2);
        }
    }
}
