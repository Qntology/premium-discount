fn main() {
    // `tauri.conf.json` points `frontendDist` at `../../web/desktop/dist`, which is gitignored.
    // On debug `cargo check` / `cargo test` invocations before the frontend has been built,
    // ensure the directory exists so `tauri::generate_context!()` does not panic.
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
            let dist = std::path::Path::new(&manifest_dir).join("../../web/desktop/dist");
            let _ = std::fs::create_dir_all(&dist);
        }
    }
    tauri_build::build()
}
