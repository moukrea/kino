// Hide the console window on Windows release builds. Windows is out of v1
// scope per PRD §2 but the attribute is a standard Tauri host convention and
// costs nothing on the targets we ship.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kino_app_lib::run();
}
