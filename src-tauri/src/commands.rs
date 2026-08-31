// Comandos expostos ao frontend (webview).
//
// Fase 1: `ping` (healthcheck da ponte JS <-> Rust).
// Fase 2: `scan_folder` (deteccao de arquivos).
// Fase 3: `read_file_bytes` (le um arquivo do disco -> ArrayBuffer no JS,
//          usado pela analise Essentia do Extended Mix).
// Fase 4: `convert_masters` (WAV->MP3 dos masters via LAME sidecar).
// Fase 7: `create_draft` + `add_draft_file` (upload) e `set_upload_cancelled`.

use crate::converter::{self, ConvertedFile, MasterInput};
use crate::scanner::{self, ScanResult};
use crate::uploader::{self, DraftResult, UploadPayload};
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
///
/// A analise so usa os primeiros 90s do audio; ler um master de 10min
/// (~1GB WAV) inteiro para a memoria (e copiar para o JS) e desperdicio.
/// Para WAV PCM canonico devolvemos so o trecho inicial, com os tamanhos
/// RIFF/data corrigidos para continuar sendo um WAV valido.
#[tauri::command]
pub fn read_file_bytes(path: String) -> Result<Response, String> {
    const ANALYSIS_SECS: u64 = 90;
    let bytes = match read_wav_head(&path, ANALYSIS_SECS) {
        Some(b) => b,
        None => fs::read(&path).map_err(|e| format!("Failed to read {}: {}", path, e))?,
    };
    Ok(Response::new(bytes))
}

/// Le so os primeiros `max_secs` de um WAV, remontando um arquivo valido
/// (header + chunks pre-data + data truncado em block_align). Devolve None se
/// nao for um WAV parseavel ou se ele ja couber no limite — o caller le tudo.
fn read_wav_head(path: &str, max_secs: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = fs::File::open(path).ok()?;
    let mut riff = [0u8; 12];
    f.read_exact(&mut riff).ok()?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return None;
    }

    let mut pre: Vec<u8> = Vec::new(); // chunks antes do "data", verbatim
    let mut byte_rate: u64 = 0;
    let mut block_align: u64 = 1;
    loop {
        let mut hdr = [0u8; 8];
        f.read_exact(&mut hdr).ok()?;
        let size = u64::from(u32::from_le_bytes(hdr[4..8].try_into().ok()?));
        if &hdr[0..4] == b"data" {
            if byte_rate == 0 {
                return None; // sem fmt antes do data: nao arriscamos cortar
            }
            let cap = (max_secs * byte_rate / block_align) * block_align;
            if size <= cap {
                return None; // audio ja e curto: caller le o arquivo inteiro
            }
            let mut data = vec![0u8; cap as usize];
            f.read_exact(&mut data).ok()?;
            let total = u32::try_from(4 + pre.len() as u64 + 8 + cap).ok()?;
            let mut out = Vec::with_capacity(12 + pre.len() + 8 + data.len());
            out.extend_from_slice(b"RIFF");
            out.extend_from_slice(&total.to_le_bytes());
            out.extend_from_slice(b"WAVE");
            out.extend_from_slice(&pre);
            out.extend_from_slice(b"data");
            out.extend_from_slice(&(cap as u32).to_le_bytes());
            out.extend_from_slice(&data);
            return Some(out);
        }
        // Chunk que nao e "data" (fmt, LIST...): copia verbatim, com padding
        // de 1 byte se o tamanho for impar (regra RIFF).
        let padded = size + (size & 1);
        if pre.len() as u64 + padded > 1_000_000 {
            return None; // header anormalmente grande: desiste do corte
        }
        let mut body = vec![0u8; padded as usize];
        f.read_exact(&mut body).ok()?;
        if &hdr[0..4] == b"fmt " && body.len() >= 16 {
            byte_rate = u64::from(u32::from_le_bytes(body[8..12].try_into().ok()?));
            block_align = u64::from(u16::from_le_bytes(body[12..14].try_into().ok()?)).max(1);
        }
        pre.extend_from_slice(&hdr);
        pre.extend_from_slice(&body);
    }
}

/// Metricas de qualidade de um WAV (peak, LUFS, silencio, vocal) para o QC
/// do padrao GPW. Streaming em thread separada (arquivos grandes).
#[tauri::command]
pub async fn qc_analyze(path: String, category: String) -> Result<crate::qc::QcAnalysis, String> {
    tauri::async_runtime::spawn_blocking(move || crate::qc::analyze(&path, &category))
        .await
        .map_err(|e| format!("QC task failed: {}", e))?
}

/// Soma o peak de todas as stems (dBFS). Passa de 0 dB se a soma clipar — o QC
/// usa isto para acusar "stems somados acima de -3 dB". Streaming em thread.
#[tauri::command]
pub async fn qc_stems_sum(paths: Vec<String>) -> Result<crate::qc::StemsSum, String> {
    tauri::async_runtime::spawn_blocking(move || crate::qc::analyze_stems_sum(&paths))
        .await
        .map_err(|e| format!("QC sum task failed: {}", e))?
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

/// Liga/desliga o cancelamento do upload em andamento. O frontend liga no
/// botao Cancel e desliga ao iniciar um novo envio; o stream em curso aborta
/// no proximo chunk.
#[tauri::command]
pub fn set_upload_cancelled(app: AppHandle, cancelled: bool) {
    use std::sync::atomic::Ordering;
    app.state::<uploader::CancelFlag>()
        .0
        .store(cancelled, Ordering::Relaxed);
}

/// Busca o perfil do produtor no site (nome + avatar) via HTTP nativo do Rust.
/// Feito aqui (e nao com fetch no webview) porque a API do GPW nao envia headers
/// de CORS — um fetch cross-origin do webview seria bloqueado. Devolve o objeto
/// `profile` cru (ou null).
#[tauri::command]
pub async fn fetch_profile(token: String) -> Result<serde_json::Value, String> {
    let url = format!("{}/api/user/profile", crate::APP_BASE_URL);
    // Com timeout, ao contrário do Client::new() que estava aqui — o uploader.rs
    // já define o seu (30 min, para uploads grandes). Sem timeout, um servidor
    // lento deixava o app à espera indefinidamente logo a seguir ao login, sem
    // erro nenhum: parecia que tinha ficado preso.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Falha ao criar o cliente HTTP: {}", e))?;
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("Falha ao buscar perfil: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Perfil indisponível (HTTP {})", resp.status().as_u16()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Resposta de perfil inválida: {}", e))?;
    Ok(body.get("profile").cloned().unwrap_or(serde_json::Value::Null))
}

// --- Cifra em repouso da sessao (DPAPI) -------------------------------------
// O auth.json era gravado em texto simples: quem tivesse acesso ao perfil
// Windows (ou um malware) copiava o ficheiro e passava a agir como o produtor
// no site. Agora o conteudo e cifrado com o DPAPI do Windows, que amarra o
// resultado a ESTA conta de utilizador: copiado para outra maquina ou aberto
// por outro utilizador, nao decifra.
//
// Limite honesto: nao protege contra codigo a correr COMO o proprio produtor
// (o Windows decifra-lhe a pedido). O que fecha e a copia do ficheiro — para
// backup, pasta sincronizada, outro perfil ou outro PC.
#[cfg(windows)]
mod crypt {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    /// Copia o resultado para um Vec e liberta a memoria que o Windows alocou.
    /// Sem o LocalFree cada gravacao/leitura perdia uns bytes para sempre.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as *mut _);
        v
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
        let mut input = blob(plain);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: ptr::null_mut() };
        let ok = unsafe {
            CryptProtectData(&mut input, ptr::null(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), 0, &mut out)
        };
        if ok == 0 { return Err("CryptProtectData falhou".into()) }
        Ok(unsafe { take(out) })
    }

    pub fn unprotect(cipher: &[u8]) -> Result<Vec<u8>, String> {
        let mut input = blob(cipher);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(&mut input, ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), 0, &mut out)
        };
        if ok == 0 { return Err("CryptUnprotectData falhou".into()) }
        Ok(unsafe { take(out) })
    }
}

// Fora do Windows nao ha DPAPI. O app so e distribuido para Windows; isto
// existe para o `cargo test`/`cargo check` continuarem a compilar noutro SO.
#[cfg(not(windows))]
mod crypt {
    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, String> { Ok(plain.to_vec()) }
    pub fn unprotect(cipher: &[u8]) -> Result<Vec<u8>, String> { Ok(cipher.to_vec()) }
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
    Ok(dir.join("auth.dat"))
}

/// Ficheiro antigo, em texto simples. So existe em instalacoes anteriores a
/// esta versao; e lido uma vez para migrar e apagado a seguir.
fn legacy_auth_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Config dir indisponível: {}", e))?;
    Ok(dir.join("auth.json"))
}

/// Grava a sessao (JSON) cifrada em disco.
#[tauri::command]
pub fn save_auth(app: AppHandle, data: String) -> Result<(), String> {
    let path = auth_file(&app)?;
    let sealed = crypt::protect(data.as_bytes())?;
    fs::write(&path, sealed).map_err(|e| format!("Falha ao salvar sessão: {}", e))?;
    // Se ainda existir o ficheiro em texto simples, desaparece agora.
    let _ = fs::remove_file(legacy_auth_file(&app)?);
    Ok(())
}

/// Le a sessao gravada (None se nao houver).
#[tauri::command]
pub fn load_auth(app: AppHandle) -> Result<Option<String>, String> {
    let path = auth_file(&app)?;
    if let Ok(sealed) = fs::read(&path) {
        // Decifrar falha se o ficheiro foi copiado de outra conta/maquina, ou
        // se estiver corrompido. Nao e erro: trata-se como "sem sessao" e o
        // produtor faz login outra vez.
        return match crypt::unprotect(&sealed) {
            Ok(plain) => Ok(String::from_utf8(plain).ok()),
            Err(_) => Ok(None),
        };
    }
    // Migracao de quem ja tinha o auth.json em texto simples: le, regrava
    // cifrado e apaga o antigo. O produtor nao da por nada (nao e deslogado).
    let legacy = legacy_auth_file(&app)?;
    match fs::read_to_string(&legacy) {
        Ok(s) => {
            let _ = save_auth(app, s.clone());
            Ok(Some(s))
        }
        Err(_) => Ok(None),
    }
}

/// Apaga a sessao gravada (logout). Apaga tambem o ficheiro antigo, para uma
/// instalacao a meio da migracao nao deixar o token em texto simples atras.
#[tauri::command]
pub fn clear_auth(app: AppHandle) -> Result<(), String> {
    let _ = fs::remove_file(auth_file(&app)?);
    let _ = fs::remove_file(legacy_auth_file(&app)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::read_wav_head;

    /// A sessao tem de voltar exatamente igual depois de cifrada e decifrada.
    /// Se isto partir, o produtor e deslogado a cada arranque do app.
    #[test]
    fn sessao_sobrevive_ao_ciclo_de_cifra() {
        let sessao = r#"{"accessToken":"eyJhbGciOi.abc","refreshToken":"r-123","expiresAt":1799999999,"email":"a@b.com"}"#;
        let selado = super::crypt::protect(sessao.as_bytes()).expect("cifrar");
        assert_ne!(selado, sessao.as_bytes(), "o conteudo tem de sair diferente do texto simples");
        let aberto = super::crypt::unprotect(&selado).expect("decifrar");
        assert_eq!(String::from_utf8(aberto).unwrap(), sessao);
    }

    /// Ficheiro corrompido ou vindo de outra conta Windows nao pode rebentar o
    /// arranque: tem de falhar, para o load_auth devolver None e pedir login.
    #[cfg(windows)]
    #[test]
    fn lixo_nao_decifra() {
        assert!(super::crypt::unprotect(b"nao sou um blob do DPAPI").is_err());
    }


    /// WAV PCM mono 8-bit sintetico: byte_rate=8 (8 bytes/segundo).
    fn wav_fixture(data_len: u32) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(4 + 24 + 8 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVE");
        w.extend_from_slice(b"fmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&1u16.to_le_bytes()); // mono
        w.extend_from_slice(&8u32.to_le_bytes()); // sample rate
        w.extend_from_slice(&8u32.to_le_bytes()); // byte rate
        w.extend_from_slice(&1u16.to_le_bytes()); // block align
        w.extend_from_slice(&8u16.to_le_bytes()); // bits
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());
        w.extend(std::iter::repeat(0xABu8).take(data_len as usize));
        w
    }

    #[test]
    fn corta_wav_longo_e_mantem_header_valido() {
        let path = std::env::temp_dir().join("gpw_test_head.wav");
        std::fs::write(&path, wav_fixture(100)).unwrap();
        // 2s * 8 bytes/s = 16 bytes de data
        let out = read_wav_head(path.to_str().unwrap(), 2).unwrap();
        assert_eq!(&out[0..4], b"RIFF");
        assert_eq!(out.len(), 12 + 24 + 8 + 16);
        let riff_size = u32::from_le_bytes(out[4..8].try_into().unwrap());
        assert_eq!(riff_size as usize, out.len() - 8);
        let data_size = u32::from_le_bytes(out[40..44].try_into().unwrap());
        assert_eq!(data_size, 16);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wav_curto_ou_invalido_devolve_none() {
        let path = std::env::temp_dir().join("gpw_test_short.wav");
        std::fs::write(&path, wav_fixture(8)).unwrap();
        // 8 bytes = 1s de audio, cabe em 90s -> None (caller le tudo)
        assert!(read_wav_head(path.to_str().unwrap(), 90).is_none());
        std::fs::write(&path, b"not a wav at all").unwrap();
        assert!(read_wav_head(path.to_str().unwrap(), 90).is_none());
        let _ = std::fs::remove_file(&path);
    }
}
