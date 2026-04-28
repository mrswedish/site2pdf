// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // On macOS, Tauri + Tokio create a multi-threaded process before Chrome is
    // launched. macOS's ObjC runtime then blocks Chrome from fork()-ing its own
    // helper processes (renderer, GPU, …) with EXC_BREAKPOINT/SIGTRAP.
    // Setting this env var before any threads are spawned disables that check
    // for this process and all children it spawns.
    #[cfg(target_os = "macos")]
    // SAFETY: called before Tokio's thread pool is created.
    unsafe { std::env::set_var("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES"); }

    site2pdf_lib::run();
}
