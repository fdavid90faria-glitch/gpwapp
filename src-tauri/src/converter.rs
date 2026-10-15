// converter.rs (Fase 4) - converte os masters WAV->MP3 via LAME (sidecar).
//
// Regra: gera 1 MP3 por master presente (Extended Mix e Radio Mix). O
// frontend monta a lista
// (MP3_MAP em app.js). Os mixdowns NAO viram MP3. O form do site so tem 2 slots
// hoje, mas o backend aceita qualquer xf_* (ver CONTRATO_UPLOAD.md).
//
// As conversoes rodam em paralelo (uma task por arquivo) e emitem o evento
// "convert:progress" para a UI. MP3 320k CBR via LAME (libmp3lame), -q 0
// (qualidade maxima de encoding).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::ShellExt;

/// Master a converter (montado pelo frontend a partir do scan).
#[derive(Deserialize, Clone)]
pub struct MasterInput {
    pub path: String,
    /// campo de upload do MP3 gerado: "xf_extended_mp3" | "xf_radio_mp3"
    pub field: String,
    /// nome do arquivo de saida, ex: "ExtendedMaster.mp3"
    pub out_name: String,
    /// rotulo legivel, ex: "Extended Mix (MP3)"
    pub label: String,
}

#[derive(Serialize, Clone)]
pub struct ConvertedFile {
    pub source: String,
    pub path: String,
    pub field: String,
    pub out_name: String,
    pub label: String,
    pub size: u64,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Clone)]
struct Progress {
    field: String,
    label: String,
    out_name: String,
    /// "start" | "done" | "error"
    status: String,
    error: Option<String>,
}

/// Pasta de saida dos MP3 (temp do SO). Cada conversao tem nome proprio dentro dela.
fn output_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("gpw-uploader").join("mp3");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Falha ao criar pasta de saída: {}", e))?;
    Ok(dir)
}

async fn convert_one(app: AppHandle, out_dir: PathBuf, m: MasterInput) -> ConvertedFile {
    let _ = app.emit(
        "convert:progress",
        Progress {
            field: m.field.clone(),
            label: m.label.clone(),
            out_name: m.out_name.clone(),
            status: "start".into(),
            error: None,
        },
    );

    // Nome UNICO por conversao. Com o nome fixo, um LAME de uma ronda antiga
    // (outra pasta, slot corrigido) ou de outra instancia do app continuava a
    // escrever no mesmo ExtendedMaster.mp3 — o MP3 enviado saia de outra track
    // ou colado de duas (auditoria 2026-10-15 #1). O site recebe o out_name,
    // nunca o nome em disco.
    // ponytail: os MP3 antigos ficam no temp (~15MB por ronda); limpar no
    // arranque se o temp do produtor comecar a encher.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let out_path = out_dir.join(format!("{}-{}-{}", std::process::id(), n, m.out_name));
    let out_str = out_path.to_string_lossy().to_string();

    // LAME (libmp3lame): WAV->MP3 320k CBR, -q 0 = maior qualidade de encoding.
    let result = async {
        let sidecar = app
            .shell()
            .sidecar("lame")
            .map_err(|e| format!("LAME sidecar indisponível: {}", e))?;
        let output = sidecar
            .args(["--cbr", "-b", "320", "-q", "0", &m.path, &out_str])
            .output()
            .await
            .map_err(|e| format!("Falha ao executar LAME: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
            return Err(format!("LAME falhou: {}", tail));
        }
        // O LAME 3.100.1 sai com 0 numa entrada que nao suporta (WAV float
        // 64-bit) e escreve um MP3 vazio de ~2KB, que passava como "Done".
        // ponytail: < 1 s a 320 kbps = saida invalida; um master real tem minutos.
        let size = std::fs::metadata(&out_path).map(|md| md.len()).unwrap_or(0);
        if size < 40_000 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let why = stderr.lines().find(|l| l.contains("supported")).unwrap_or("unsupported input format");
            return Err(format!("LAME produced an empty MP3 ({} B): {}", size, why.trim()));
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => {
            let size = std::fs::metadata(&out_path).map(|md| md.len()).unwrap_or(0);
            let _ = app.emit(
                "convert:progress",
                Progress {
                    field: m.field.clone(),
                    label: m.label.clone(),
                    out_name: m.out_name.clone(),
                    status: "done".into(),
                    error: None,
                },
            );
            ConvertedFile {
                source: m.path,
                path: out_str,
                field: m.field,
                out_name: m.out_name,
                label: m.label,
                size,
                ok: true,
                error: None,
            }
        }
        Err(err) => {
            let _ = app.emit(
                "convert:progress",
                Progress {
                    field: m.field.clone(),
                    label: m.label.clone(),
                    out_name: m.out_name.clone(),
                    status: "error".into(),
                    error: Some(err.clone()),
                },
            );
            ConvertedFile {
                source: m.path,
                path: out_str,
                field: m.field,
                out_name: m.out_name,
                label: m.label,
                size: 0,
                ok: false,
                error: Some(err),
            }
        }
    }
}

/// Converte todos os masters em paralelo e devolve os resultados.
pub async fn convert(app: AppHandle, masters: Vec<MasterInput>) -> Result<Vec<ConvertedFile>, String> {
    let out_dir = output_dir()?;

    let mut handles = Vec::new();
    for m in masters {
        let app_c = app.clone();
        let dir_c = out_dir.clone();
        handles.push(tauri::async_runtime::spawn(convert_one(app_c, dir_c, m)));
    }

    let mut results = Vec::new();
    for h in handles {
        match h.await {
            Ok(cf) => results.push(cf),
            Err(e) => return Err(format!("Tarefa de conversão falhou: {}", e)),
        }
    }
    Ok(results)
}
