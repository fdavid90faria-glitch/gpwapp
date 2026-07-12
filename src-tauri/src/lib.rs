// GPW Uploader - Tauri backend entrypoint.
//
// Modulos:
//   scanner.rs   (Fase 2) - identifica arquivos pela nomenclatura
//   converter.rs (Fase 4) - WAV->MP3 via FFmpeg sidecar
//   uploader.rs  (Fase 7) - envio multipart para /api/tracks/upload

mod commands;
mod converter;
mod qc;
mod scanner;
mod uploader;

/// URL base do site GPW (producao). O app envia upload/login para aqui.
pub const APP_BASE_URL: &str = "https://ghostproducerworld.com";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(uploader::CancelFlag(std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(false),
        )))
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::scan_folder,
            commands::read_file_bytes,
            commands::qc_analyze,
            commands::qc_stems_sum,
            commands::convert_masters,
            commands::create_draft,
            commands::add_draft_file,
            commands::set_upload_cancelled,
            commands::fetch_profile,
            commands::save_auth,
            commands::load_auth,
            commands::clear_auth
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
