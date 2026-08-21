#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// The bin shares the lib's [dependencies] table, so every slopcast_lib
// dependency looks unused to this target. The lint still runs on the lib.
#![allow(
    unused_crate_dependencies,
    reason = "dependencies are used by slopcast_lib"
)]

fn main() {
    slopcast_lib::run();
}
