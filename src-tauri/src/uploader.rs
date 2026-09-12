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
// TODOS os ficheiros vao por aqui, mesmo os pequenos (1 pedaco). Havia um
// caminho separado de PUT unico abaixo de 96MB: dois caminhos para a mesma
// coisa, e as correcoes caiam num so (a validacao do rascunho ficou meses
// so no PUT unico). Um ficheiro pequeno custa 2 pedidos de controlo a mais.
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

// ── PONTE DE TOKEN (Rust ↔ JS) ───────────────────────────────
// O Rust nao renova sessoes: a sessao Supabase vive no JS (supabase.js). Um
// token apanhado no inicio de um ficheiro expira (~1h) a meio de um upload de
// horas — o `sign` da parte 21 dava 401 com o produtor logado, e o `abort`
// seguinte tambem, deixando as partes no R2. Aqui o Rust PEDE um token fresco
// ao JS antes de cada chamada de controlo: emite `auth:token-needed`, o JS
// responde por `provide_token`. Sem resposta em 10s, segue com o que tinha.
pub struct TokenBridge(pub std::sync::Mutex<Option<tokio::sync::oneshot::Sender<String>>>);
impl Default for TokenBridge {
    fn default() -> Self {
        Self(std::sync::Mutex::new(None))
    }
}

pub fn provide_token(app: &AppHandle, token: String) {
    if let Ok(mut slot) = app.state::<TokenBridge>().0.lock() {
        if let Some(tx) = slot.take() {
            let _ = tx.send(token);
        }
    }
}

async fn fresh_token(app: &AppHandle, fallback: &str) -> String {
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    if let Ok(mut slot) = app.state::<TokenBridge>().0.lock() {
        *slot = Some(tx);
    }
    let _ = app.emit("auth:token-needed", ());
    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(t)) if !t.is_empty() => t,
        _ => fallback.to_string(),
    }
}

// ── TETO POR SLOT ─────────────────────────────────────────────
// ESPELHO de lib/upload-exts.js (maxBytesFor) no site — se mudar la, muda
// aqui. Verificado ANTES de enviar um byte: sem isto um WAV de 1.2GB subia
// inteiro (~80 min a 2 Mbps) para o servidor o recusar no `complete`.
fn max_bytes_for(fkey: &str) -> u64 {
    if fkey == "stems" { 2 * 1024 * 1024 * 1024 } else { 1024 * 1024 * 1024 }
}
fn max_label_for(fkey: &str) -> &'static str {
    if fkey == "stems" { "2GB" } else { "1GB" }
}

// ── CHAMADAS DE CONTROLO ──────────────────────────────────────
const CONTROL_ATTEMPTS: u32 = 3;
const CONTROL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
fn backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(2 * attempt as u64)
}

/// Erro de uma chamada ao servidor ou ao R2: status HTTP (0 = rede/local) +
/// mensagem para o produtor. O status chega ao frontend em DraftResult —
/// antes o caminho em pedacos colapsava tudo em 0.
#[derive(Debug, Clone)]
struct ApiError {
    status: u16,
    message: String,
}
fn cancelled() -> ApiError {
    ApiError { status: 0, message: "Upload cancelled.".into() }
}

/// POST JSON com Bearer, partilhado por draft-file-url, upload-multipart e
/// draft-file-commit. Tentativas em rede/5xx/429 (todas estas chamadas sao
/// idempotentes do lado do servidor); 4xx e deterministico e sai logo. Token
/// FRESCO em cada tentativa (ver fresh_token).
async fn post_json(
    app: &AppHandle,
    client: &reqwest::Client,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let url = format!("{}{}", APP_BASE_URL, path);
    let mut last = ApiError { status: 0, message: String::new() };
    for attempt in 1..=CONTROL_ATTEMPTS {
        if is_cancelled(app) {
            return Err(cancelled());
        }
        let tok = fresh_token(app, token).await;
        // Timeout PROPRIO: o cliente e' partilhado com os PUTs (30 min). Uma
        // chamada JSON pendurada bloqueava o cancelamento por meia hora.
        match client
            .post(&url)
            .timeout(CONTROL_TIMEOUT)
            .bearer_auth(&tok)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                let json: serde_json::Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
                let ok = status.is_success() && json.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
                if ok {
                    return Ok(json);
                }
                let message = json
                    .get("error")
                    .and_then(|e| e.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| status_message(status.as_u16()));
                last = ApiError { status: status.as_u16(), message };
                if status.is_client_error() && status.as_u16() != 429 {
                    return Err(last);
                }
            }
            Err(e) => {
                last = ApiError { status: 0, message: format!("Falha de rede: {}", e) };
            }
        }
        if attempt < CONTROL_ATTEMPTS {
            tokio::time::sleep(backoff(attempt)).await;
        }
    }
    Err(last)
}

/// Corpo (reqwest::Body) do ficheiro INTEIRO em streaming, com progresso.
/// Usado pelo multipart/form-data do create_draft. E' o part_body de offset 0.
async fn progress_body(app: &AppHandle, uf: &UploadFile) -> Result<(reqwest::Body, u64), String> {
    let total = tokio::fs::metadata(&uf.path)
        .await
        .map_err(|e| format!("{}: nao foi possivel ler ({})", uf.filename, e))?
        .len();
    Ok((part_body(app, uf, 0, total, total).await?, total))
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

/// Abre o ficheiro posicionado em `offset` e limitado a `len` bytes. Separado
/// do part_body para ser TESTAVEL sem AppHandle: e' aqui que um erro de seek
/// ou de take sobe um ficheiro corrompido em silencio.
async fn open_range(
    path: &str,
    offset: u64,
    len: u64,
) -> Result<tokio::io::Take<tokio::fs::File>, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut f = tokio::fs::File::open(path).await?;
    f.seek(std::io::SeekFrom::Start(offset)).await?;
    // take(len) garante que este pedaco NAO invade o seguinte.
    Ok(f.take(len))
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
    let limited = open_range(&uf.path, offset, len)
        .await
        .map_err(|e| format!("{}: falha ao abrir ({})", uf.filename, e))?;

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

const MP_PATH: &str = "/api/tracks/upload-multipart";

fn field_str(v: &serde_json::Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// PUT de UM pedaco, com tentativas. Regras — as MESMAS que o site aplica no
/// browser:
///  · cancelamento do produtor sai ja, sem repetir;
///  · 403/404 e' assinatura expirada (portatil adormeceu a meio) → pede URL NOVA
///    para esta parte em vez de repetir a morta;
///  · outros 4xx nao mudam com repeticao; rede/5xx/429 repetem com backoff;
///  · abrir/posicionar o ficheiro tambem entra no retry — um antivirus ou o
///    OneDrive a tocar no ZIP acabado de exportar da um erro transitorio.
#[allow(clippy::too_many_arguments)]
async fn put_part(
    app: &AppHandle,
    client: &reqwest::Client,
    mut url: String,
    path: &str,
    upload_id: &str,
    part_n: u32,
    token: &str,
    file: &UploadFile,
    offset: u64,
    len: u64,
    total_file: u64,
) -> Result<(), ApiError> {
    let mut last = ApiError { status: 0, message: String::new() };
    for attempt in 1..=PART_ATTEMPTS {
        if is_cancelled(app) {
            return Err(cancelled());
        }
        // Ficheiro mudou de tamanho a meio (re-export do DAW por cima)? Sem
        // isto o hyper falhava com "body write aborted" e nos repetiamos 3x
        // um pedaco que nunca vai bater com o Content-Length prometido.
        if let Ok(m) = tokio::fs::metadata(&file.path).await {
            if m.len() != total_file {
                return Err(ApiError {
                    status: 0,
                    message: format!("{}: file changed during upload — export it again and retry.", file.filename),
                });
            }
        }
        let body = match part_body(app, file, offset, len, total_file).await {
            Ok(b) => b,
            Err(m) => {
                last = ApiError { status: 0, message: m };
                if attempt < PART_ATTEMPTS {
                    tokio::time::sleep(backoff(attempt)).await;
                }
                continue;
            }
        };
        match client
            .put(&url)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(body)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => {
                let st = r.status().as_u16();
                last = ApiError { status: st, message: format!("Storage rejected a chunk (HTTP {}).", st) };
                match st {
                    403 | 404 => {
                        let j = post_json(
                            app,
                            client,
                            token,
                            MP_PATH,
                            serde_json::json!({ "action": "sign", "path": path, "uploadId": upload_id, "parts": [part_n] }),
                        )
                        .await?;
                        let fresh = j
                            .get("urls")
                            .and_then(|u| u.as_array())
                            .and_then(|a| a.first())
                            .map(|item| field_str(item, "url"))
                            .unwrap_or_default();
                        if fresh.is_empty() {
                            last.message = "Upload em pedacos: assinatura invalida.".into();
                            return Err(last);
                        }
                        url = fresh;
                    }
                    429 => {}
                    400..=499 => return Err(last),
                    _ => {}
                }
            }
            Err(e) => {
                if is_cancelled(app) {
                    return Err(cancelled());
                }
                last = ApiError { status: 0, message: format!("Falha de rede no upload: {}", e) };
            }
        }
        if attempt < PART_ATTEMPTS {
            tokio::time::sleep(backoff(attempt)).await;
        }
    }
    Err(last)
}

/// Sobe todos os pedacos, pedindo as assinaturas em lotes (URLs sempre frescas
/// num upload longo). Verifica o cancelamento entre lotes — entre partes nao
/// ha stream nenhum a detecta-lo.
async fn put_all_parts(
    app: &AppHandle,
    client: &reqwest::Client,
    token: &str,
    file: &UploadFile,
    path: &str,
    upload_id: &str,
    total: u64,
) -> Result<(), ApiError> {
    let n_parts = part_count(total);
    for first in (1..=n_parts).step_by(SIGN_BATCH as usize) {
        if is_cancelled(app) {
            return Err(cancelled());
        }
        let last_n = (first + SIGN_BATCH - 1).min(n_parts);
        let nums: Vec<u32> = (first..=last_n).collect();
        let signed = post_json(
            app,
            client,
            token,
            MP_PATH,
            serde_json::json!({ "action": "sign", "path": path, "uploadId": upload_id, "parts": nums }),
        )
        .await?;
        let urls = signed
            .get("urls")
            .and_then(|u| u.as_array())
            .cloned()
            .unwrap_or_default();
        if urls.len() != nums.len() {
            return Err(ApiError { status: 500, message: "Upload em pedacos: o servidor devolveu assinaturas a menos.".into() });
        }
        for (item, expected) in urls.iter().zip(&nums) {
            let part_n = item.get("part").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let url = field_str(item, "url");
            // A parte tem de ser a que pedimos, pela ordem: uma URL da parte 3
            // usada para os bytes da parte 4 monta um ficheiro corrompido.
            if part_n != *expected || url.is_empty() {
                return Err(ApiError { status: 500, message: "Upload em pedacos: assinatura invalida.".into() });
            }
            let (offset, len) = part_range(part_n, total)
                .ok_or_else(|| ApiError { status: 500, message: "Upload em pedacos: pedaco fora do ficheiro.".into() })?;
            put_part(app, client, url, path, upload_id, part_n, token, file, offset, len, total).await?;
        }
    }
    Ok(())
}

/// Sobe um ficheiro grande em pedacos e devolve o path final no R2.
///
/// Fluxo LINEAR de proposito: create → partes → complete dentro de um so
/// Result, e QUALQUER falha depois do create cancela no R2. A versao anterior
/// tinha o complete dentro do ramo Ok com `?` — saia antes do abort, e a
/// unica falha que acontece com 2GB ja no R2 era precisamente a que deixava
/// as partes la para sempre (revisao de 2026-09-12).
async fn put_in_parts(
    app: &AppHandle,
    client: &reqwest::Client,
    token: &str,
    file: &UploadFile,
    fkey: &str,
    total: u64,
) -> Result<String, ApiError> {
    let created = post_json(
        app,
        client,
        token,
        MP_PATH,
        serde_json::json!({ "action": "create", "fkey": fkey, "filename": file.filename }),
    )
    .await?;
    let upload_id = field_str(&created, "uploadId");
    let path = field_str(&created, "path");
    if upload_id.is_empty() || path.is_empty() {
        return Err(ApiError { status: 500, message: "Upload em pedacos: resposta incompleta do servidor.".into() });
    }

    let result: Result<(), ApiError> = async {
        put_all_parts(app, client, token, file, &path, &upload_id, total).await?;
        if is_cancelled(app) {
            return Err(cancelled());
        }
        let done = post_json(
            app,
            client,
            token,
            MP_PATH,
            serde_json::json!({ "action": "complete", "path": path, "uploadId": upload_id }),
        )
        .await?;
        // O servidor junta as partes que o R2 LISTA, nao as que enviamos. Se
        // faltar uma (PUT que respondeu 200 sem gravar), o objeto fica com um
        // buraco — e' aqui que se apanha, nao quando o comprador abre o ZIP.
        let joined = done.get("parts").and_then(|p| p.as_u64()).unwrap_or(0);
        if joined != part_count(total) as u64 {
            return Err(ApiError {
                status: 500,
                message: format!("Upload em pedacos: o servidor juntou {} de {} pedacos.", joined, part_count(total)),
            });
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => Ok(path),
        Err(e) => {
            // Senao os pedacos ja enviados ficam a ocupar espaco faturavel no
            // R2, invisiveis na listagem do bucket.
            let _ = post_json(
                app,
                client,
                token,
                MP_PATH,
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

impl DraftResult {
    fn fail(status: u16, message: String) -> Self {
        DraftResult { ok: false, status, id: None, message, warnings: vec![] }
    }
}

/// Anexa UM arquivo a um rascunho por UPLOAD DIRETO ao R2 (3 passos):
///   1) /api/app/draft-file-url  -> valida rascunho (dono/estado) e slot em <1s,
///      ANTES de enviar um byte
///   2) envio DIRETO ao R2 (nao passa pelo backend nem pelo proxy do Cloudflare,
///      que corta > 100MB), sempre em pedacos de 64MB (put_in_parts), com
///      streaming + Content-Length.
///   3) /api/app/draft-file-commit -> regista em metadata.files / original_url
///
/// Emite progresso por arquivo; o token e' renovado pelo JS a meio (fresh_token).
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

    // Teto do slot ANTES de gastar banda: o servidor tambem mede no fim, mas
    // descobrir isso depois de 80 min de upload nao ajuda ninguem.
    if file_size > max_bytes_for(&fkey) {
        let msg = format!("{}: file too large (max {}).", file.filename, max_label_for(&fkey));
        progress(&app, "error", msg.clone());
        return Ok(DraftResult::fail(413, msg));
    }

    // 1) draft-file-url — so para VALIDAR o rascunho (dono/estado) e o slot em
    //    <1s, antes de gastar banda. A URL de PUT que devolve nao se usa (nao
    //    cria objeto nenhum): o envio e' sempre em pedacos.
    if let Err(e) = post_json(
        &app,
        &client,
        &token,
        "/api/app/draft-file-url",
        serde_json::json!({ "draft_id": draft_id, "fkey": fkey, "filename": file.filename }),
    )
    .await
    {
        progress(&app, "error", e.message.clone());
        return Ok(DraftResult::fail(e.status, e.message));
    }

    // 2) Envio direto ao R2, em pedacos (um so caminho, do cover ao ZIP de 2GB).
    let path = match put_in_parts(&app, &client, &token, &file, &fkey, file_size).await {
        Ok(p) => p,
        Err(e) => {
            // Cancelamento pelo flag, nao pelo texto da mensagem: o frontend
            // distingue Err (cancelou) de DraftResult{ok:false} (falhou).
            if is_cancelled(&app) {
                progress(&app, "error", "Upload cancelled.".into());
                return Err("Upload cancelled.".into());
            }
            progress(&app, "error", e.message.clone());
            return Ok(DraftResult::fail(e.status, e.message));
        }
    };

    // 3) Confirma no site — regista o ficheiro em metadata.files[fkey].
    commit_file(&app, &client, &token, &draft_id, &fkey, &path, &file.filename).await
}

/// Passo final, comum aos dois caminhos (PUT unico e pedacos): regista o
/// ficheiro no rascunho. Sem isto o ficheiro fica no R2 sem pertencer a nada.
async fn commit_file(
    app: &AppHandle,
    client: &reqwest::Client,
    token: &str,
    draft_id: &str,
    fkey: &str,
    path: &str,
    filename: &str,
) -> Result<DraftResult, String> {
    match post_json(
        app,
        client,
        token,
        "/api/app/draft-file-commit",
        serde_json::json!({ "draft_id": draft_id, "fkey": fkey, "path": path, "name": filename }),
    )
    .await
    {
        Ok(_) => Ok(DraftResult { ok: true, status: 200, id: None, message: fkey.to_string(), warnings: vec![] }),
        Err(e) => {
            progress(app, "error", e.message.clone());
            Ok(DraftResult::fail(e.status, e.message))
        }
    }
}

/// A CSP do webview e o Essentia estao amarrados um ao outro: o Emscripten/
/// embind usa `new Function(...)`, que exige 'unsafe-eval'. Enquanto o motor
/// corria NA PAGINA, tirar o 'unsafe-eval' matava a detecao de BPM/Key em
/// silencio — o app mostrava "Could not auto-detect" e ninguem ligava a causa
/// a CSP (aconteceu na build 3.11.0). A saida foi a mesma do site: o motor
/// corre num worker, que nao recebe CSP (o Tauri so poe o header nas respostas
/// .html), e a pagina fecha o eval. Estes testes prendem as tres pecas juntas.
#[cfg(test)]
mod csp_vs_essentia {
    use std::path::PathBuf;

    fn ler(rel: &str) -> String {
        let p: PathBuf = [env!("CARGO_MANIFEST_DIR"), rel].iter().collect();
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("nao li {:?}: {}", p, e))
    }

    /// A CSP viva (app.security.csp), lida do JSON e nao por substring do
    /// ficheiro — um comentario ou uma chave morta com 'unsafe-eval' passava.
    fn diretiva(nome: &str) -> String {
        let conf: serde_json::Value = serde_json::from_str(&ler("tauri.conf.json")).expect("tauri.conf.json invalido");
        let csp = conf["app"]["security"]["csp"].as_str().expect("app.security.csp em falta");
        csp.split(';')
            .map(str::trim)
            .find(|d| d.starts_with(nome) && d[nome.len()..].starts_with(' '))
            .unwrap_or_else(|| panic!("diretiva {} em falta na CSP", nome))
            .to_string()
    }

    /// Um token exato da diretiva (nao substring: 'wasm-unsafe-eval' CONTEM
    /// 'unsafe-eval' e passaria a verificacao por substring).
    fn tem_token(diretiva_nome: &str, token: &str) -> bool {
        diretiva(diretiva_nome).split_whitespace().any(|t| t == token)
    }

    #[test]
    fn o_essentia_corre_no_worker_e_a_pagina_fecha_o_eval() {
        let glue = ler("../src/essentia/essentia-wasm.web.js");
        assert!(
            glue.contains("new Function") || glue.contains(" eval("),
            "o glue do Essentia ja nao usa new Function — este arranjo todo deixou de ser preciso"
        );

        // 1) A pagina nao carrega o motor: se carregasse, precisava de eval.
        let html = ler("../src/index.html");
        for script in ["essentia-wasm.web.js", "essentia.js-core.js"] {
            assert!(
                !html.contains(&format!("src=\"/essentia/{}\"", script)),
                "{} carregado na pagina: volta a precisar de 'unsafe-eval'",
                script
            );
        }

        // 2) O motor corre no worker (que nao recebe CSP nenhuma).
        let worker = ler("../src/essentia/gpw-essentia-worker.js");
        assert!(worker.contains("importScripts"), "o worker tem de carregar o glue com importScripts");
        assert!(worker.contains("essentia-wasm.web.js"), "o worker nao importa o glue do Essentia");
        assert!(
            ler("../src/essentia/gpw-analyzer.js").contains("gpw-essentia-worker.js"),
            "o analyzer tem de arrancar o worker"
        );

        // 3) E por isso a CSP da pagina pode ficar fechada ao eval.
        assert!(
            !tem_token("script-src", "'unsafe-eval'"),
            "script-src com 'unsafe-eval': se foi reposto, o motor voltou para a pagina — \
             passa-o para o worker em vez de abrir a CSP"
        );
    }

    #[test]
    fn a_csp_continua_a_fechar_o_resto() {
        // Repor o 'unsafe-eval' nao pode servir de desculpa para abrir tudo.
        assert_eq!(diretiva("default-src"), "default-src 'self'", "a base da CSP tem de continuar 'self'");
        for d in ["script-src", "connect-src"] {
            let v = diretiva(d);
            assert!(!v.split(' ').any(|s| s == "*"), "{} aberto a tudo: {}", d, v);
        }
    }
}

#[cfg(test)]
mod part_math {
    use super::{max_bytes_for, open_range, part_count, part_range, PART_SIZE};

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
            1,                        // ficheiro minusculo: 1 pedaco so
            5 * 1024 * 1024,          // o minimo de um pedaco do S3 (aqui e' o ultimo)
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
    /// open_range (o codigo de PRODUCAO, nao uma copia) le mesmo esses bytes.
    /// Usa o mesmo part_range com um ficheiro pequeno: o mecanismo e' igual a
    /// 64MB, mas nao ha maneira de ter PART_SIZE pequeno so no teste, por isso
    /// o ladrilhar e' testado acima e aqui prova-se o seek+take por offset.
    #[tokio::test]
    async fn open_range_reconstroi_o_ficheiro_byte_a_byte() {
        use tokio::io::AsyncReadExt;

        let original: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let dir = std::env::temp_dir().join(format!("gpw-part-test-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let caminho = dir.join("amostra.bin");
        tokio::fs::write(&caminho, &original).await.unwrap();
        let caminho = caminho.to_str().unwrap().to_string();

        const PEDACO: u64 = 3_000; // ultimo pedaco fica com 1.000 (resto)
        let total = original.len() as u64;
        let mut reconstruido: Vec<u8> = Vec::new();
        let mut offset = 0u64;
        while offset < total {
            let len = PEDACO.min(total - offset);
            let mut buf = Vec::new();
            open_range(&caminho, offset, len).await.unwrap().read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf.len() as u64, len, "o take leu alem do pedaco (offset {})", offset);
            reconstruido.extend_from_slice(&buf);
            offset += len;
        }
        assert_eq!(reconstruido, original, "o ficheiro remontado difere do original");

        // Pedir alem do fim nao inventa bytes: devolve so o que existe.
        let mut buf = Vec::new();
        open_range(&caminho, total - 10, 1_000).await.unwrap().read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, &original[original.len() - 10..]);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// O teto de partes que conta e' o do SERVIDOR (maxPartsFor em
    /// upload-multipart/route.js = ceil(teto do slot / 5MB)), nao os 10.000 do
    /// S3. Se o PART_SIZE descer, o ficheiro maximo do slot deixa de caber.
    #[test]
    fn ficheiro_no_teto_do_slot_cabe_no_limite_de_partes_do_servidor() {
        const MIN_PART: u64 = 5 * 1024 * 1024;
        assert!(PART_SIZE >= MIN_PART, "o R2 recusa pedacos abaixo de 5MB (exceto o ultimo)");
        for fkey in ["stems", "master", "project"] {
            let teto = max_bytes_for(fkey);
            let max_parts_servidor = teto.div_ceil(MIN_PART) as u32;
            assert!(
                part_count(teto) <= max_parts_servidor,
                "{}: {} pedacos > {} que o servidor assina",
                fkey, part_count(teto), max_parts_servidor
            );
        }
        // Espelho de lib/upload-exts.js: stems 2GB, resto 1GB.
        assert_eq!(max_bytes_for("stems"), 2 * 1024 * 1024 * 1024);
        assert_eq!(max_bytes_for("master"), 1024 * 1024 * 1024);
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
