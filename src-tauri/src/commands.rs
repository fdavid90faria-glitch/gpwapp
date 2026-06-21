// Comandos expostos ao frontend (webview).
//
// Fase 1: `ping` (healthcheck da ponte JS <-> Rust).
// Fase 2: `scan_folder` (deteccao de arquivos).
// Fase 3: `read_file_bytes` (le um arquivo do disco -> ArrayBuffer no JS,
//          usado pela analise Essentia do Extended Mix).
// Fase 4: `convert_masters` (WAV->MP3 dos masters via FFmpeg sidecar).
// Fases seguintes adicionam: upload_track, etc.

use crate::converter::{self, ConvertedFile, MasterInput};
use crate::scanner::{self, ScanResult};
use crate::uploader::{self, DraftResult, UploadPayload, UploadResult};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use tauri::ipc::Response;
use tauri::{AppHandle, Manager};

#[derive(Serialize)]
pub struct PingResult {
    pub ok: bool,
    pub message: String,
    pub version: String,
}

/// Healthcheck simples chamado pelo frontend ao iniciar.
#[tauri::command]
pub fn ping() -> PingResult {
    PingResult {
        ok: true,
        message: "GPW Uploader backend ready".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Escaneia a pasta de exportacao e classifica cada arquivo (Fase 2).
#[tauri::command]
pub fn scan_folder(folder: String) -> Result<ScanResult, String> {
    scanner::scan(&folder)
}

/// Le um arquivo do disco e devolve os bytes brutos (vira ArrayBuffer no JS).
/// Usado pela analise de BPM/Key (Fase 3) para alimentar o Essentia sem ter
/// um objeto File. Resposta binaria eficiente via tauri::ipc::Response.
#[tauri::command]
pub fn read_file_bytes(path: String) -> Result<Response, String> {
    let bytes = fs::read(&path).map_err(|e| format!("Failed to read {}: {}", path, e))?;
    Ok(Response::new(bytes))
}

/// Converte os masters WAV->MP3 via FFmpeg sidecar (Fase 4). Emite
/// "convert:progress" por arquivo e devolve os MP3 gerados.
#[tauri::command]
pub async fn convert_masters(
    app: AppHandle,
    masters: Vec<MasterInput>,
) -> Result<Vec<ConvertedFile>, String> {
    converter::convert(app, masters).await
}

/// Envia a track completa (master + extras + metadata) para o GPW (Fase 7).
/// Emite "upload:progress" e devolve o resultado da API.
#[tauri::command]
pub async fn upload_track(app: AppHandle, payload: UploadPayload) -> Result<UploadResult, String> {
    uploader::upload(app, payload).await
}

/// Cria um rascunho no site (sobe os arquivos) e devolve o id, para o produtor
/// finalizar em upload.html?edit=<id>. Caminho principal do fluxo atual.
#[tauri::command]
pub async fn create_draft(app: AppHandle, payload: UploadPayload) -> Result<DraftResult, String> {
    uploader::create_draft(app, payload).await
}

/// Anexa um arquivo ao rascunho (um por requisicao — evita 502 em uploads grandes).
#[tauri::command]
pub async fn add_draft_file(
    app: AppHandle,
    token: String,
    draft_id: String,
    file: crate::uploader::UploadFile,
) -> Result<DraftResult, String> {
    uploader::add_file(app, token, draft_id, file).await
}

/// Busca o perfil do produtor no site (nome + avatar) via HTTP nativo do Rust.
/// Feito aqui (e nao com fetch no webview) porque a API do GPW nao envia headers
/// de CORS — um fetch cross-origin do webview seria bloqueado. Devolve o objeto
/// `profile` cru (ou null).
#[tauri::command]
pub async fn fetch_profile(token: String) -> Result<serde_json::Value, String> {
    let url = format!("{}/api/user/profile", crate::APP_BASE_URL);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("Falha ao buscar perfil: {}", e))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Resposta de perfil inválida: {}", e))?;
    Ok(body.get("profile").cloned().unwrap_or(serde_json::Value::Null))
}

// --- Persistencia da sessao (Fase 5) ---------------------------------------
// Guardada em arquivo na pasta de config do app (sobrevive a fechar/reabrir,
// ao contrario do localStorage do webview). O frontend renova o token no boot.

fn auth_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Config dir indisponível: {}", e))?;
    fs::create_dir_all(&dir).map_err(|e| format!("Falha ao criar config dir: {}", e))?;
    Ok(dir.join("auth.json"))
}

/// Grava a sessao (JSON) em disco.
#[tauri::command]
pub fn save_auth(app: AppHandle, data: String) -> Result<(), String> {
    let path = auth_file(&app)?;
    fs::write(&path, data).map_err(|e| format!("Falha ao salvar sessão: {}", e))
}

/// Le a sessao gravada (None se nao houver).
#[tauri::command]
pub fn load_auth(app: AppHandle) -> Result<Option<String>, String> {
    let path = auth_file(&app)?;
    match fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(_) => Ok(None),
    }
}

/// Apaga a sessao gravada (logout).
#[tauri::command]
pub fn clear_auth(app: AppHandle) -> Result<(), String> {
    let path = auth_file(&app)?;
    let _ = fs::remove_file(&path);
    Ok(())
}
