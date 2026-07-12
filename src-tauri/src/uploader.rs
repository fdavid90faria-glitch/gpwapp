// uploader.rs (Fase 7) - envia a track para o GPW via multipart/form-data.
//
// Fluxo atual (ver CONTRATO_UPLOAD.md): `create_draft` sobe master+cover e
// cria o rascunho; `add_file` anexa os demais arquivos um a um. Header
// `Authorization: Bearer <access_token>`, body = FormData.
//
// Os arquivos sao transmitidos em streaming direto do disco (nao carregamos
// 800MB em memoria). A resposta e devolvida crua ao frontend. O upload em
// andamento pode ser cancelado via CancelFlag (comando set_upload_cancelled).

use crate::APP_BASE_URL;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::io::ReaderStream;

/// Flag global de cancelamento (state gerenciado pelo Tauri). O frontend liga
/// via `set_upload_cancelled`; o stream de upload em andamento aborta no
/// proximo chunk.
pub struct CancelFlag(pub Arc<AtomicBool>);

fn is_cancelled(app: &AppHandle) -> bool {
    app.state::<CancelFlag>().0.load(Ordering::Relaxed)
}

/// Um arquivo a anexar no multipart.
#[derive(Deserialize, Clone)]
pub struct UploadFile {
    /// nome do campo no form: "file" | "cover" | "xf_extended_mp3" | ...
    pub field: String,
    /// caminho absoluto no disco
    pub path: String,
    /// nome do arquivo enviado (ex: "Track - Extended Mix.wav")
    pub filename: String,
}

/// Payload completo montado pelo frontend (tela de revisao).
#[derive(Deserialize)]
pub struct UploadPayload {
    pub token: String,
    /// pares (campo, valor) de texto: title, genre, bpm, price_eur, metadata...
    pub fields: Vec<(String, String)>,
    pub files: Vec<UploadFile>,
}

#[derive(Serialize, Clone)]
struct UploadProgress {
    /// "preparing" | "uploading" | "done" | "error"
    stage: String,
    message: String,
}

/// Progresso de bytes de um arquivo durante o upload (emitido por chunk,
/// throttle a cada 1% para nao inundar a UI).
#[derive(Serialize, Clone)]
struct FileProgress {
    field: String,
    filename: String,
    sent: u64,
    total: u64,
    percent: u64,
}

fn mime_for(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "zip" => "application/zip",
        "mid" | "midi" => "audio/midi",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

/// Corpo (reqwest::Body) que transmite o arquivo em streaming (sem carregar tudo
/// em memoria) e emite "upload:file-progress" conforme os bytes sao enviados.
/// Devolve tambem o tamanho total (para o Content-Length). Partilhado pelo
/// multipart (create_draft) e pelo PUT direto ao R2 (add_file).
async fn progress_body(app: &AppHandle, uf: &UploadFile) -> Result<(reqwest::Body, u64), String> {
    let meta = tokio::fs::metadata(&uf.path)
        .await
        .map_err(|e| format!("{}: nao foi possivel ler ({})", uf.filename, e))?;
    let total = meta.len();

    let file = tokio::fs::File::open(&uf.path)
        .await
        .map_err(|e| format!("{}: falha ao abrir ({})", uf.filename, e))?;

    // Conta os bytes enviados e emite o progresso (throttle por 1%).
    // Se o cancelamento for pedido, injeta um erro -> o reqwest aborta o envio.
    let cancel = app.state::<CancelFlag>().0.clone();
    let app_c = app.clone();
    let field = uf.field.clone();
    let filename = uf.filename.clone();
    let mut sent: u64 = 0;
    let mut last_pct: u64 = u64::MAX;
    let stream = ReaderStream::new(file).map(move |chunk| {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "upload cancelled",
            ));
        }
        if let Ok(ref bytes) = chunk {
            sent += bytes.len() as u64;
            let percent = if total > 0 { (sent * 100 / total).min(100) } else { 100 };
            if percent != last_pct {
                last_pct = percent;
                let _ = app_c.emit(
                    "upload:file-progress",
                    FileProgress { field: field.clone(), filename: filename.clone(), sent, total, percent },
                );
            }
        }
        chunk
    });
    Ok((reqwest::Body::wrap_stream(stream), total))
}

/// Constroi um Part multipart que transmite o arquivo em streaming.
async fn file_part(app: &AppHandle, uf: &UploadFile) -> Result<reqwest::multipart::Part, String> {
    let (body, total) = progress_body(app, uf).await?;
    let ext = Path::new(&uf.filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    reqwest::multipart::Part::stream_with_length(body, total)
        .file_name(uf.filename.clone())
        .mime_str(mime_for(ext))
        .map_err(|e| format!("{}: mime invalido ({})", uf.filename, e))
}

/// Monta o multipart (reabrindo os arquivos em streaming) e envia uma vez.
/// Cada tentativa precisa de um Form novo porque o stream e consumido no envio.
async fn send_once(
    app: &AppHandle,
    client: &reqwest::Client,
    url: &str,
    payload: &UploadPayload,
) -> Result<reqwest::Response, String> {
    let mut form = reqwest::multipart::Form::new();
    for (k, v) in &payload.fields {
        form = form.text(k.clone(), v.clone());
    }
    for uf in &payload.files {
        let part = file_part(app, uf).await?;
        form = form.part(uf.field.clone(), part);
    }

    client
        .post(url)
        .bearer_auth(&payload.token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Falha de rede no upload: {}", e))
}

/// Resultado da criacao de rascunho (Fase "continuar no site").
#[derive(Serialize, Clone)]
pub struct DraftResult {
    pub ok: bool,
    pub status: u16,
    /// id do rascunho criado (track com status=draft)
    pub id: Option<String>,
    pub message: String,
    pub warnings: Vec<String>,
}

fn progress(app: &AppHandle, stage: &str, message: String) {
    let _ = app.emit(
        "upload:progress",
        UploadProgress {
            stage: stage.to_string(),
            message,
        },
    );
}

/// Valida os arquivos e cria o cliente HTTP (timeout longo p/ uploads grandes).
async fn prepare(app: &AppHandle, files: &[UploadFile]) -> Result<reqwest::Client, String> {
    progress(app, "preparing", "Preparing files…".into());
    for uf in files {
        if tokio::fs::metadata(&uf.path).await.is_err() {
            let msg = format!("File not found: {}", uf.filename);
            progress(app, "error", msg.clone());
            return Err(msg);
        }
    }
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| format!("Falha ao criar cliente HTTP: {}", e))
}

/// Envia o multipart com retry em falhas de rede (ate 3x, backoff). Erros HTTP
/// (400/401/409...) NAO sao repetidos — sao deterministicos.
async fn post_with_retry(
    app: &AppHandle,
    client: &reqwest::Client,
    url: &str,
    payload: &UploadPayload,
) -> Result<reqwest::Response, String> {
    const MAX_ATTEMPTS: u32 = 3;
    let mut last_err = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let label = if attempt == 1 {
            "Uploading to Ghost Producer World…".to_string()
        } else {
            format!("Network issue — retrying ({}/{})…", attempt, MAX_ATTEMPTS)
        };
        progress(app, "uploading", label);

        match send_once(app, client, url, payload).await {
            Ok(r) => return Ok(r),
            Err(e) => {
                if is_cancelled(app) {
                    progress(app, "error", "Upload cancelled.".into());
                    return Err("Upload cancelled.".into());
                }
                last_err = e;
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                }
            }
        }
    }
    progress(app, "error", last_err.clone());
    Err(last_err)
}

fn status_message(status: u16) -> String {
    match status {
        401 => "Session expired — log out and log in again.".into(),
        403 => "Your account is not verified to upload yet.".into(),
        409 => "You already have a track with this name.".into(),
        413 => "File too large for the server.".into(),
        429 => "Too many uploads — try again later.".into(),
        s => format!("Upload failed (HTTP {}).", s),
    }
}

fn warnings_of(body: &serde_json::Value) -> Vec<String> {
    body.get("warnings")
        .and_then(|w| w.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Cria um RASCUNHO no site (/api/app/draft-create): sobe os arquivos + dados
/// conhecidos e devolve o id. O produtor finaliza abrindo upload.html?edit=<id>.
pub async fn create_draft(app: AppHandle, payload: UploadPayload) -> Result<DraftResult, String> {
    let client = prepare(&app, &payload.files).await?;
    let url = format!("{}/api/app/draft-create", APP_BASE_URL);
    let resp = post_with_retry(&app, &client, &url, &payload).await?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
    let warnings = warnings_of(&body);

    if status.is_success() && body.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
        let id = body
            .get("id")
            .map(|id| id.to_string().trim_matches('"').to_string());
        progress(&app, "done", "Draft ready — opening the site…".into());
        Ok(DraftResult { ok: true, status: status.as_u16(), id, message: "ok".into(), warnings })
    } else {
        let message = body
            .get("error")
            .and_then(|e| e.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| status_message(status.as_u16()));
        progress(&app, "error", message.clone());
        Ok(DraftResult { ok: false, status: status.as_u16(), id: None, message, warnings })
    }
}

/// Anexa UM arquivo a um rascunho por UPLOAD DIRETO ao R2 (3 passos):
///   1) /api/app/draft-file-url  -> pede uma URL assinada de PUT (JSON pequeno)
///   2) PUT do arquivo DIRETO para o R2 (nao passa pelo backend nem pelo proxy
///      do Cloudflare, que corta uploads > 100MB) — streaming + Content-Length
///   3) /api/app/draft-file-commit -> confirma e regista em metadata.files
/// Emite progresso por arquivo e tenta de novo em falha de rede (ate 3x no PUT).
pub async fn add_file(
    app: AppHandle,
    token: String,
    draft_id: String,
    file: UploadFile,
) -> Result<DraftResult, String> {
    if tokio::fs::metadata(&file.path).await.is_err() {
        let msg = format!("File not found: {}", file.filename);
        progress(&app, "error", msg.clone());
        return Err(msg);
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| format!("Falha ao criar cliente HTTP: {}", e))?;
    // O master (campo "file") vai para o slot "master" (o commit poe-no em
    // original_url); os extras usam o proprio fkey sem o prefixo "xf_".
    let fkey = if file.field == "file" {
        "master".to_string()
    } else {
        file.field.strip_prefix("xf_").unwrap_or(&file.field).to_string()
    };

    let err_result = |status: u16, message: String| {
        Ok(DraftResult { ok: false, status, id: None, message, warnings: vec![] })
    };

    // 1) Pede a URL assinada de PUT. Payload minusculo — passa pelo Cloudflare.
    let url_endpoint = format!("{}/api/app/draft-file-url", APP_BASE_URL);
    let meta_resp = client
        .post(&url_endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "draft_id": draft_id, "fkey": fkey, "filename": file.filename }))
        .send()
        .await
        .map_err(|e| format!("Falha de rede ao preparar o upload: {}", e))?;
    let meta_status = meta_resp.status();
    let meta_body: serde_json::Value = meta_resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
    if !meta_status.is_success() || !meta_body.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
        let message = meta_body.get("error").and_then(|e| e.as_str()).map(String::from)
            .unwrap_or_else(|| status_message(meta_status.as_u16()));
        return err_result(meta_status.as_u16(), message);
    }
    let put_url = meta_body.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
    let path = meta_body.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
    if put_url.is_empty() || path.is_empty() {
        return err_result(500, "Upload URL missing.".into());
    }

    // 2) PUT do arquivo DIRETO para o R2. Streaming + Content-Length (o R2 exige-o).
    //    Retry em falha de rede ou 5xx; um 4xx e deterministico (nao repete).
    const MAX_ATTEMPTS: u32 = 3;
    let mut last_err = String::new();
    let mut put_status = 0u16;
    let mut put_ok = false;
    for attempt in 1..=MAX_ATTEMPTS {
        let (body, total) = progress_body(&app, &file).await?; // stream novo a cada tentativa
        match client
            .put(&put_url)
            .header(reqwest::header::CONTENT_LENGTH, total)
            .body(body)
            .send()
            .await
        {
            Ok(r) => {
                put_status = r.status().as_u16();
                if r.status().is_success() {
                    put_ok = true;
                    break;
                }
                last_err = format!("Storage rejected the file (HTTP {}).", put_status);
                if attempt < MAX_ATTEMPTS && r.status().is_server_error() {
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                } else {
                    break; // 4xx nao adianta repetir
                }
            }
            Err(e) => {
                if is_cancelled(&app) {
                    progress(&app, "error", "Upload cancelled.".into());
                    return Err("Upload cancelled.".into());
                }
                last_err = format!("Falha de rede no upload: {}", e);
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                }
            }
        }
    }
    if !put_ok {
        progress(&app, "error", last_err.clone());
        return err_result(put_status, if last_err.is_empty() { "Upload failed.".into() } else { last_err });
    }

    // 3) Confirma no site — regista o ficheiro em metadata.files[fkey].
    let commit_endpoint = format!("{}/api/app/draft-file-commit", APP_BASE_URL);
    let commit_resp = client
        .post(&commit_endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "draft_id": draft_id, "fkey": fkey, "path": path, "name": file.filename }))
        .send()
        .await
        .map_err(|e| format!("Falha de rede ao confirmar o upload: {}", e))?;
    let commit_status = commit_resp.status();
    let commit_body: serde_json::Value = commit_resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
    if commit_status.is_success() && commit_body.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
        Ok(DraftResult { ok: true, status: commit_status.as_u16(), id: None, message: fkey, warnings: vec![] })
    } else {
        let message = commit_body.get("error").and_then(|e| e.as_str()).map(String::from)
            .unwrap_or_else(|| status_message(commit_status.as_u16()));
        err_result(commit_status.as_u16(), message)
    }
}

#[cfg(test)]
mod tests {
    // Integração opcional: confirma que um PUT em STREAMING com Content-Length
    // (o que o add_file faz) é aceite pelo R2 — o R2 recusa Transfer-Encoding:
    // chunked com 411. Só corre se R2_TEST_PUT_URL estiver definida (gerar com
    // scripts _gen-put-url.mjs no repo do site). Sem a env var, passa a saltar.
    use futures_util::StreamExt;

    #[tokio::test]
    async fn direct_put_stream_has_content_length() {
        let url = match std::env::var("R2_TEST_PUT_URL") {
            Ok(u) if !u.is_empty() => u,
            _ => return, // sem URL de teste -> skip
        };
        let data = vec![0x47u8; 3 * 1024 * 1024]; // 3 MB
        let total = data.len() as u64;
        // Stream em chunks de 64KB, como o ReaderStream de um ficheiro real.
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
            data.chunks(64 * 1024).map(|c| Ok(c.to_vec())).collect();
        let stream = futures_util::stream::iter(chunks).boxed();
        let body = reqwest::Body::wrap_stream(stream);

        let res = reqwest::Client::new()
            .put(&url)
            .header(reqwest::header::CONTENT_LENGTH, total)
            .body(body)
            .send()
            .await
            .expect("PUT falhou (rede)");
        assert_eq!(res.status().as_u16(), 200, "R2 recusou o PUT em streaming: {}", res.status());
    }
}
