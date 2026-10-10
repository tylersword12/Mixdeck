// Desktop entry point; the app itself lives in lib.rs (shared with Android).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    mixdeck_lib::run()
}
