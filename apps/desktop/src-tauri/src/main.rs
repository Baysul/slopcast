#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(
    unused_crate_dependencies,
    reason = "dependencies are used by slopcast_lib"
)]

fn main() {
    slopcast_lib::run();
}
