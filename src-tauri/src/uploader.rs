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

// ── UPLOAD EM PEDACOS (multipart do R2) ──────────────────────
// Um PUT unico de um ficheiro grande morre com qualquer soluco de rede, e o
// retry que ja existia reenviava o ficheiro INTEIRO desde o inicio. Pior: o
// cliente tem timeout de 30 min, e 2GB numa ligacao de 2 Mbps levam ~140 min —
// abortava sempre, por melhor que a ligacao estivesse.
//
// Em pedacos: cada PUT e pequeno (cabe no timeout com folga) e uma falha
// reenvia so aquele pedaco. Usa a MESMA rota que o site (validada contra o R2
// real): /api/tracks/upload-multipart. O servidor nao precisou de mudar nada.
const MULTIPART_MIN: u64 = 96 * 1024 * 1024; // abaixo disto, PUT unico como antes
const PART_SIZE: u64 = 64 * 1024 * 1024; // minimo do S3/R2 e 5MB
const SIGN_BATCH: u32 = 10; // assinaturas pedidas de cada vez (o servidor aceita ate 50)
const PART_ATTEMPTS: u32 = 3;

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

/// Intervalo [offset, len) do pedaco `part_n` (1-based). None se o pedaco cai
/// fora do ficheiro. Funcao PURA de proposito: e aqui que mora o unico erro que
/// nao da erro nenhum — um offset trocado sobe um ficheiro corrompido em
/// silencio. Os testes no fim do ficheiro cobrem-na.
fn part_range(part_n: u32, total: u64) -> Option<(u64, u64)> {
    if part_n == 0 {
        return None;
    }
    let offset = (part_n as u64 - 1) * PART_SIZE;
    if offset >= total {
        return None;
    }
    Some((offset, PART_SIZE.min(total - offset)))
}

/// Quantos pedacos para um ficheiro deste tamanho.
fn part_count(total: u64) -> u32 {
    total.div_ceil(PART_SIZE) as u32
}

/// Corpo de UM pedaco: le so o intervalo [offset, offset+len) do ficheiro, em
/// streaming. O progresso emitido e o do FICHEIRO inteiro (offset + enviados),
/// senao a barra saltava para 0% a cada pedaco novo.
async fn part_body(
    app: &AppHandle,
    uf: &UploadFile,
    offset: u64,
    len: u64,
    total_file: u64,
) -> Result<reqwest::Body, String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut f = tokio::fs::File::open(&uf.path)
        .await
        .map_err(|e| format!("{}: falha ao abrir ({})", uf.filename, e))?;
    f.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|e| format!("{}: falha ao posicionar ({})", uf.filename, e))?;
    // take(len) garante que este pedaco NAO invade o seguinte.
    let limited = f.take(len);

    let cancel = app.state::<CancelFlag>().0.clone();
    let app_c = app.clone();
    let field = uf.field.clone();
    let filename = uf.filename.clone();
    let mut sent: u64 = 0;
    let mut last_pct: u64 = u64::MAX;
    let stream = ReaderStream::new(limited).map(move |chunk| {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "upload cancelled",
            ));
        }
        if let Ok(ref bytes) = chunk {
            sent += bytes.len() as u64;
            let done = offset + sent;
            let percent = if total_file > 0 { (done * 100 / total_file).min(100) } else { 100 };
            if percent != last_pct {
                last_pct = percent;
                let _ = app_c.emit(
                    "upload:file-progress",
                    FileProgress {
                        field: field.clone(),
                        filename: filename.clone(),
                        sent: done,
                        total: total_file,
                        percent,
                    },
                );
            }
        }
        chunk
    });
    Ok(reqwest::Body::wrap_stream(stream))
}

/// Chamada JSON a /api/tracks/upload-multipart (create/sign/complete/abort).
async fn mp_call(
    client: &reqwest::Client,
    token: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("{}/api/tracks/upload-multipart", APP_BASE_URL);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Falha de rede no upload em pedacos: {}", e))?;
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
    if !status.is_success() || !json.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
        let msg = json
            .get("error")
            .and_then(|e| e.as_str())
            .map(String::from)
            .unwrap_or_else(|| status_message(status.as_u16()));
        return Err(msg);
    }
    Ok(json)
}

/// PUT de UM pedaco, com tentativas. Um 4xx e deterministico e nao repete; o
/// cancelamento do produtor sai imediatamente.
async fn put_one_part(
    app: &AppHandle,
    client: &reqwest::Client,
    url: &str,
    file: &UploadFile,
    offset: u64,
    len: u64,
    total_file: u64,
) -> Result<(), String> {
    let mut last_err = String::new();
    for attempt in 1..=PART_ATTEMPTS {
        let body = part_body(app, file, offset, len, total_file).await?;
        let sent = client
            .put(url)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(body)
            .send()
            .await;
        match sent {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => {
                let st = r.status();
                last_err = format!("Storage rejected a chunk (HTTP {}).", st.as_u16());
                if !st.is_server_error() {
                    break; // 4xx nao adianta repetir
                }
            }
            Err(e) => {
                if is_cancelled(app) {
                    return Err("Upload cancelled.".into());
                }
                last_err = format!("Falha de rede no upload: {}", e);
            }
        }
        if attempt < PART_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
        }
    }
    Err(last_err)
}

/// Sobe todos os pedacos, pedindo as assinaturas em lotes (URLs sempre frescas
/// num upload longo).
async fn put_all_parts(
    app: &AppHandle,
    client: &reqwest::Client,
    token: &str,
    file: &UploadFile,
    path: &str,
    upload_id: &str,
    total: u64,
    n_parts: u32,
) -> Result<(), String> {
    let mut n = 1u32;
    while n <= n_parts {
        let last = (n + SIGN_BATCH - 1).min(n_parts);
        let nums: Vec<u32> = (n..=last).collect();
        let signed = mp_call(
            client,
            token,
            serde_json::json!({ "action": "sign", "path": path, "uploadId": upload_id, "parts": nums }),
        )
        .await?;
        let urls = signed
            .get("urls")
            .and_then(|u| u.as_array())
            .cloned()
            .unwrap_or_default();
        if urls.len() != nums.len() {
            return Err("Upload em pedacos: o servidor devolveu assinaturas a menos.".into());
        }
        for item in urls {
            let part_n = item.get("part").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
            if part_n == 0 || url.is_empty() {
                return Err("Upload em pedacos: assinatura invalida.".into());
            }
            let (offset, len) = part_range(part_n, total)
                .ok_or_else(|| "Upload em pedacos: pedaco fora do ficheiro.".to_string())?;
            put_one_part(app, client, url, file, offset, len, total).await?;
        }
        n = last + 1;
    }
    Ok(())
}

/// Sobe um ficheiro grande em pedacos e devolve o path final no R2.
/// Em qualquer falha, cancela o upload no R2 — senao os pedacos ja enviados
/// ficam a ocupar espaco faturavel, invisiveis na listagem do bucket.
async fn put_in_parts(
    app: &AppHandle,
    client: &reqwest::Client,
    token: &str,
    file: &UploadFile,
    fkey: &str,
    total: u64,
) -> Result<String, String> {
    let created = mp_call(
        client,
        token,
        serde_json::json!({ "action": "create", "fkey": fkey, "filename": file.filename }),
    )
    .await?;
    let upload_id = created
        .get("uploadId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let path = created
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if upload_id.is_empty() || path.is_empty() {
        return Err("Upload em pedacos: resposta incompleta do servidor.".into());
    }

    let n_parts = part_count(total);
    match put_all_parts(app, client, token, file, &path, &upload_id, total, n_parts).await {
        Ok(()) => {
            mp_call(
                client,
                token,
                serde_json::json!({ "action": "complete", "path": path, "uploadId": upload_id }),
            )
            .await?;
            Ok(path)
        }
        Err(e) => {
            let _ = mp_call(
                client,
                token,
                serde_json::json!({ "action": "abort", "path": path, "uploadId": upload_id }),
            )
            .await;
            Err(e)
        }
    }
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
    let file_size = match tokio::fs::metadata(&file.path).await {
        Ok(m) => m.len(),
        Err(_) => {
            let msg = format!("File not found: {}", file.filename);
            progress(&app, "error", msg.clone());
            return Err(msg);
        }
    };
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

    // FICHEIRO GRANDE: vai em pedacos e salta os passos 1 e 2 abaixo (o
    // create/sign/complete trata de tudo e devolve o path final).
    if file_size > MULTIPART_MIN {
        return match put_in_parts(&app, &client, &token, &file, &fkey, file_size).await {
            Ok(path) => commit_file(&app, &client, &token, &draft_id, &fkey, &path, &file.filename).await,
            Err(e) => {
                progress(&app, "error", e.clone());
                if e.contains("cancelled") {
                    return Err(e);
                }
                err_result(0, e)
            }
        };
    }

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
    commit_file(&app, &client, &token, &draft_id, &fkey, &path, &file.filename).await
}

/// Passo final, comum aos dois caminhos (PUT unico e pedacos): regista o
/// ficheiro no rascunho. Sem isto o ficheiro fica no R2 sem pertencer a nada.
async fn commit_file(
    _app: &AppHandle,
    client: &reqwest::Client,
    token: &str,
    draft_id: &str,
    fkey: &str,
    path: &str,
    filename: &str,
) -> Result<DraftResult, String> {
    let commit_endpoint = format!("{}/api/app/draft-file-commit", APP_BASE_URL);
    let commit_resp = client
        .post(&commit_endpoint)
        .bearer_auth(token)
        .json(&serde_json::json!({ "draft_id": draft_id, "fkey": fkey, "path": path, "name": filename }))
        .send()
        .await
        .map_err(|e| format!("Falha de rede ao confirmar o upload: {}", e))?;
    let commit_status = commit_resp.status();
    let commit_body: serde_json::Value = commit_resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
    if commit_status.is_success() && commit_body.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
        Ok(DraftResult { ok: true, status: commit_status.as_u16(), id: None, message: fkey.to_string(), warnings: vec![] })
    } else {
        let message = commit_body.get("error").and_then(|e| e.as_str()).map(String::from)
            .unwrap_or_else(|| status_message(commit_status.as_u16()));
        Ok(DraftResult { ok: false, status: commit_status.as_u16(), id: None, message, warnings: vec![] })
    }
}

/// A CSP do webview e o glue do Essentia estao amarrados um ao outro: o
/// Emscripten/embind usa `new Function(...)`, que exige 'unsafe-eval'. Trocar
/// por 'wasm-unsafe-eval' (que so cobre WebAssembly) mata a detecao de BPM/Key
/// em silencio — o app mostra "Could not auto-detect" e ninguem liga a causa a
/// CSP. Aconteceu: a troca foi feita num hardening, ficou meses por lancar, e
/// so estourou quando saiu na build 3.11.0.
#[cfg(test)]
mod csp_vs_essentia {
    use std::path::PathBuf;

    fn ler(rel: &str) -> String {
        let p: PathBuf = [env!("CARGO_MANIFEST_DIR"), rel].iter().collect();
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("nao li {:?}: {}", p, e))
    }

    #[test]
    fn se_o_essentia_usa_new_function_a_csp_tem_de_permitir_eval() {
        let glue = ler("../src/essentia/essentia-wasm.web.js");
        let precisa_eval = glue.contains("new Function") || glue.contains(" eval(");
        let csp = ler("tauri.conf.json");
        if precisa_eval {
            assert!(
                csp.contains("'unsafe-eval'"),
                "o glue do Essentia usa new Function, logo a CSP TEM de ter 'unsafe-eval'. \
                 Com 'wasm-unsafe-eval' apenas, a detecao de BPM/Key morre em silencio."
            );
        }
    }

    #[test]
    fn a_csp_continua_a_fechar_o_resto() {
        // Repor o 'unsafe-eval' nao pode servir de desculpa para abrir tudo.
        let csp = ler("tauri.conf.json");
        assert!(csp.contains("default-src 'self'"), "a base da CSP tem de continuar 'self'");
        assert!(!csp.contains("script-src *"), "script-src aberto a tudo");
        assert!(!csp.contains("connect-src *"), "connect-src aberto a tudo");
    }
}

#[cfg(test)]
mod part_math {
    use super::{part_count, part_range, MULTIPART_MIN, PART_SIZE};

    /// Os pedacos tem de LADRILHAR o ficheiro: sem buracos, sem sobreposicao, e
    /// a soma tem de dar o tamanho exato. Um erro aqui nao rebenta — sobe um
    /// ficheiro corrompido que so se descobre quando o comprador o abre.
    fn assert_tiles(total: u64) {
        let n = part_count(total);
        let mut esperado_offset = 0u64;
        let mut soma = 0u64;
        for p in 1..=n {
            let (offset, len) = part_range(p, total)
                .unwrap_or_else(|| panic!("pedaco {} de {} bytes devia existir", p, total));
            assert_eq!(offset, esperado_offset, "buraco/sobreposicao no pedaco {} ({} bytes)", p, total);
            assert!(len > 0, "pedaco {} vazio ({} bytes)", p, total);
            assert!(len <= PART_SIZE, "pedaco {} maior que o teto ({} bytes)", p, total);
            esperado_offset += len;
            soma += len;
        }
        assert_eq!(soma, total, "a soma dos pedacos nao da o ficheiro ({} bytes)", total);
        // Um pedaco a mais nao pode existir: era um PUT vazio no fim.
        assert!(part_range(n + 1, total).is_none(), "pedaco a mais em {} bytes", total);
    }

    #[test]
    fn pedacos_ladrilham_o_ficheiro() {
        for total in [
            MULTIPART_MIN + 1,        // o mais pequeno que vai por pedacos
            PART_SIZE,                // exatamente 1 pedaco
            PART_SIZE + 1,            // 2 pedacos, o 2o com 1 byte
            PART_SIZE * 2,            // exato, sem resto
            PART_SIZE * 3 + 12345,    // resto qualquer
            1_298_361_000,            // ~1.3GB, o caso real que originou isto
            2 * 1024 * 1024 * 1024,   // 2GB, o teto dos stems
        ] {
            assert_tiles(total);
        }
    }

    #[test]
    fn ultimo_pedaco_leva_o_resto() {
        let total = PART_SIZE * 2 + 7;
        assert_eq!(part_range(3, total), Some((PART_SIZE * 2, 7)));
        assert_eq!(part_count(total), 3);
    }

    #[test]
    fn pedaco_invalido_ou_fora_do_ficheiro_nao_existe() {
        assert_eq!(part_range(0, 1000), None, "nao ha pedaco 0 (o S3 conta de 1)");
        assert_eq!(part_range(2, PART_SIZE), None, "ficheiro de 1 pedaco nao tem 2o");
    }

    /// A aritmetica acima diz QUE intervalos enviar; falta provar que o
    /// seek+take le mesmo esses bytes. Usa intervalos pequenos (o mecanismo e o
    /// mesmo a 64MB) e reconstroi o ficheiro a partir dos pedacos.
    #[tokio::test]
    async fn seek_e_take_reconstroem_o_ficheiro_byte_a_byte() {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        let original: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let dir = std::env::temp_dir().join(format!("gpw-part-test-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let caminho = dir.join("amostra.bin");
        tokio::fs::write(&caminho, &original).await.unwrap();

        const PEDACO: u64 = 3_000; // ultimo pedaco fica com 1.000 (resto)
        let total = original.len() as u64;
        let mut reconstruido: Vec<u8> = Vec::new();
        let mut offset = 0u64;
        while offset < total {
            let len = PEDACO.min(total - offset);
            let mut f = tokio::fs::File::open(&caminho).await.unwrap();
            f.seek(std::io::SeekFrom::Start(offset)).await.unwrap();
            let mut buf = Vec::new();
            f.take(len).read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf.len() as u64, len, "o take leu alem do pedaco (offset {})", offset);
            reconstruido.extend_from_slice(&buf);
            offset += len;
        }
        assert_eq!(reconstruido, original, "o ficheiro remontado difere do original");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn ficheiro_de_2gb_cabe_no_limite_de_10000_pedacos_do_s3() {
        assert!(part_count(2 * 1024 * 1024 * 1024) <= 10_000);
        // E o teto de um pedaco respeita o minimo de 5MB do S3/R2 (exceto o ultimo).
        assert!(PART_SIZE >= 5 * 1024 * 1024);
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
