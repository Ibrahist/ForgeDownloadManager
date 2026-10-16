fn main() {
    let manifest = tauri_build::AppManifest::new().commands(&[
        "engine_mode",
        "list_downloads",
        "add_download",
        "pause_download",
        "resume_download",
        "remove_download",
        "download_segments",
        "get_settings",
        "set_setting",
        "default_save_dir",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to run tauri-build");
}
