// scanner.rs (Fase 2) - identifica os arquivos de uma pasta de exportacao pela
// nomenclatura e mapeia cada um para o campo do upload do GPW.
//
// Regras: secao 3 da arquitetura. O mapa de campos vem do CONTRATO_UPLOAD.md
// (lido do upload.html do site). A ordem de verificacao importa: o mais
// especifico vence (instrumental+mixdown antes de instrumental, mixdown antes
// de mix/master).

use serde::Serialize;
use std::fs;
use std::path::Path;

/// Um arquivo classificado dentro da pasta de exportacao.
#[derive(Serialize, Clone)]
pub struct ScannedFile {
    pub filename: String,
    pub path: String,
    pub ext: String,
    pub size: u64,
    /// slug interno da categoria (ex: "extended_mix", "stems", "undefined")
    pub category: String,
    /// rotulo legivel (ex: "Extended Mix", "Stems")
    pub label: String,
    /// master | mixdown | instrumental | support | image | mp3 | undefined
    pub role: String,
    /// campo no multipart do upload (ex: "file", "xf_stems"); None se indefinido
    pub upload_field: Option<String>,
    /// true para os mixes master (Extended/Radio) que geram MP3 na Fase 4
    pub is_master: bool,
}

#[derive(Serialize)]
pub struct ScanResult {
    pub folder: String,
    pub files: Vec<ScannedFile>,
    pub undefined_count: usize,
    pub has_extended_master: bool,
}

struct Classification {
    category: &'static str,
    label: &'static str,
    role: &'static str,
    upload_field: Option<&'static str>,
    is_master: bool,
}

const UNDEFINED: Classification = Classification {
    category: "undefined",
    label: "Unidentified",
    role: "undefined",
    upload_field: None,
    is_master: false,
};

/// Classifica um WAV de audio pela nomenclatura (secao 3).
fn classify_wav(name_lower: &str) -> Classification {
    let radio = name_lower.contains("radio");
    let instrumental = name_lower.contains("instrumental");
    // "Unmastered" e sinonimo de mixdown — e a palavra que a copy do site usa
    // ("the unmastered mixdown"), por isso e a que muitos produtores poem no
    // nome. Como "unmastered" CONTEM "master", sem isto um
    // "...-extended-unmastered.wav" caia no slot do Extended Mix MASTER (o
    // campo `file`): o mixdown era enviado como se fosse o master. O mixdown e
    // sempre testado antes do master_kw nos dois ramos, por isso basta aqui.
    let mixdown = name_lower.contains("mixdown") || name_lower.contains("unmaster");
    // "mixdown" tambem contem "mix", por isso o mixdown e checado antes.
    let master_kw = name_lower.contains("master") || name_lower.contains("mix");

    if radio {
        // Radio Instrumental/Mixdown: mesmo padrao do Extended (secao acima).
        // Checados antes do master_kw — para nao caírem por engano no slot de
        // Radio Mix master (ambos contem "mix"). Radio Mix continua o unico
        // obrigatorio do grupo Radio (grupo todo vira obrigatorio se qualquer
        // um deles for enviado — regra aplicada no site, nao aqui).
        if instrumental && mixdown {
            Classification { category: "radio_instrumental_mixdown", label: "Radio Instrumental Mixdown", role: "mixdown", upload_field: Some("xf_radio_instrumental_mixdown"), is_master: false }
        } else if instrumental {
            Classification { category: "radio_instrumental", label: "Radio Instrumental Master", role: "instrumental", upload_field: Some("xf_radio_instrumental"), is_master: false }
        } else if mixdown {
            Classification { category: "radio_mixdown", label: "Radio Mixdown", role: "mixdown", upload_field: Some("xf_radio_mixdown"), is_master: false }
        } else if master_kw {
            Classification { category: "radio_mix", label: "Radio Mix (master)", role: "master", upload_field: Some("xf_radio_mix"), is_master: true }
        } else {
            UNDEFINED
        }
    } else {
        // Nao-radio => tratado como Extended.
        if instrumental && mixdown {
            Classification { category: "extended_instrumental_mixdown", label: "Extended Instrumental Mixdown", role: "mixdown", upload_field: Some("xf_extended_instrumental_mixdown"), is_master: false }
        } else if instrumental {
            Classification { category: "extended_instrumental", label: "Extended Instrumental", role: "instrumental", upload_field: Some("xf_extended_instrumental"), is_master: false }
        } else if mixdown {
            Classification { category: "extended_mixdown", label: "Extended Mixdown", role: "mixdown", upload_field: Some("xf_extended_mixdown"), is_master: false }
        } else if master_kw {
            // Extended Mix master => campo principal `file`.
            Classification { category: "extended_mix", label: "Extended Mix (master)", role: "master", upload_field: Some("file"), is_master: true }
        } else {
            UNDEFINED
        }
    }
}

/// Classifica um arquivo qualquer (audio, zip, projeto, cover...).
/// `in_stems_dir` = true se algum diretorio pai contem "stem".
fn classify(name_lower: &str, ext: &str, in_stems_dir: bool) -> Classification {
    match ext {
        // WAV dentro de pasta de stems e stem, nunca master ("Lead Mix.wav"
        // numa pasta Stems/ cairia no slot do Extended Mix sem este guard).
        "wav" if in_stems_dir => Classification {
            category: "stems",
            label: "Stems",
            role: "support",
            upload_field: Some("xf_stems"),
            is_master: false,
        },
        "wav" => classify_wav(name_lower),

        "mp3" => {
            // MP3 ja pronto na pasta (raro - normalmente o app gera). Mapeia
            // pelos 2 unicos slots do site.
            if name_lower.contains("radio") {
                Classification { category: "radio_mp3", label: "Radio Mix (MP3)", role: "mp3", upload_field: Some("xf_radio_mp3"), is_master: false }
            } else {
                Classification { category: "extended_mp3", label: "Extended Mix (MP3)", role: "mp3", upload_field: Some("xf_extended_mp3"), is_master: false }
            }
        }

        "mid" | "midi" => Classification { category: "midi", label: "MIDI", role: "support", upload_field: Some("xf_midi"), is_master: false },

        "flp" | "als" | "alp" | "logic" | "logicx" | "cpr" | "ableton" | "ptx" | "song" | "bwproject" =>
            Classification { category: "project", label: "Project File", role: "support", upload_field: Some("xf_project"), is_master: false },

        "jpg" | "jpeg" | "png" | "webp" =>
            Classification { category: "cover", label: "Cover", role: "image", upload_field: Some("cover"), is_master: false },

        "pdf" => Classification { category: "license", label: "License (PDF)", role: "support", upload_field: Some("xf_license"), is_master: false },

        "mp4" => Classification { category: "video", label: "Video", role: "support", upload_field: Some("xf_video"), is_master: false },

        "zip" => {
            if name_lower.contains("stem") || in_stems_dir {
                Classification { category: "stems", label: "Stems (ZIP)", role: "support", upload_field: Some("xf_stems"), is_master: false }
            } else if name_lower.contains("midi") || name_lower.contains("mid") {
                Classification { category: "midi", label: "MIDI (ZIP)", role: "support", upload_field: Some("xf_midi"), is_master: false }
            } else if name_lower.contains("project") || name_lower.contains("proj") {
                Classification { category: "project", label: "Project (ZIP)", role: "support", upload_field: Some("xf_project"), is_master: false }
            } else {
                UNDEFINED
            }
        }

        _ => {
            // Sem extensao util: pode ser stem solto dentro de pasta "stems".
            if in_stems_dir {
                Classification { category: "stems", label: "Stems", role: "support", upload_field: Some("xf_stems"), is_master: false }
            } else {
                UNDEFINED
            }
        }
    }
}

fn ext_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default()
}

/// Lixo que o Mac mete nos ZIPs: a pasta `__MACOSX/` com um "._Nome.wav" por
/// cada ficheiro (AppleDouble — metadados do Finder, nao audio). Lido como
/// stem, o QC dava "no RIFF tag found" em cada um. Espelho do GPW ANALYZER
/// (scanner::is_mac_junk, PR #5), onde reprovou 4 tracks em 2026-09.
fn is_mac_junk(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|c| c.eq_ignore_ascii_case("__MACOSX") || c.starts_with("._"))
}

/// Percorre a pasta recursivamente (profundidade limitada) coletando arquivos.
fn walk(dir: &Path, in_stems_dir: bool, depth: usize, out: &mut Vec<ScannedFile>) {
    if depth > 5 {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        let name_lower = file_name.to_lowercase();

        if path.is_dir() {
            // Ignora pastas ocultas e de sistema.
            if name_lower.starts_with('.') {
                continue;
            }
            let child_in_stems = in_stems_dir || name_lower.contains("stem");
            walk(&path, child_in_stems, depth + 1, out);
            continue;
        }

        // Ignora arquivos ocultos.
        if name_lower.starts_with('.') {
            continue;
        }

        let ext = ext_of(&file_name);
        let c = classify(&name_lower, &ext, in_stems_dir);
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);

        out.push(ScannedFile {
            filename: file_name.clone(),
            path: path.to_string_lossy().to_string(),
            ext: ext.clone(),
            size,
            category: c.category.to_string(),
            label: c.label.to_string(),
            role: c.role.to_string(),
            upload_field: c.upload_field.map(|s| s.to_string()),
            is_master: c.is_master,
        });

        // Stems num .zip: o zip acima continua a ser o ficheiro enviado
        // (xf_stems). Extraimos os WAVs a MAIS, so para o QC os analisar (soma,
        // duracao, stem vazia) — marcados sem upload_field para nao colidirem
        // com o zip nem serem enviados. Falha na extracao e ignorada (o upload
        // do zip nao depende disto).
        if ext == "zip" && (name_lower.contains("stem") || in_stems_dir) {
            if let Ok(wavs) = extract_wavs_from_zip(&path.to_string_lossy()) {
                for w in wavs {
                    let wname = Path::new(&w)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let wsize = fs::metadata(&w).map(|m| m.len()).unwrap_or(0);
                    out.push(ScannedFile {
                        filename: wname,
                        path: w,
                        ext: "wav".into(),
                        size: wsize,
                        category: "stems".into(),
                        label: "Stem (from zip)".into(),
                        role: "skip".into(), // QC-only: nao entra no upload
                        upload_field: None,
                        is_master: false,
                    });
                }
            }
        }
    }
}

/// Extrai os WAVs de um .zip de stems para uma pasta temporaria e devolve os
/// caminhos. So para QC (o zip original e que sobe no upload). Porta do
/// GPW ANALYZER (extract_wavs_from_zip), com protecao contra nomes com pasta.
fn extract_wavs_from_zip(zip_path: &str) -> Result<Vec<String>, String> {
    let path = Path::new(zip_path);
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "stems".into());
    let file = fs::File::open(path).map_err(|e| format!("open zip: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("bad zip: {}", e))?;

    let dest = std::env::temp_dir().join("gpw_uploader_stems").join(&name);
    let _ = fs::remove_dir_all(&dest);
    fs::create_dir_all(&dest).map_err(|e| format!("temp dir: {}", e))?;

    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ename = entry.name().to_string();
        if !ename.to_lowercase().ends_with(".wav") || is_mac_junk(&ename) {
            continue;
        }
        let base = ename.rsplit(['/', '\\']).next().unwrap_or(&ename).to_string();
        let target = dest.join(&base);
        let mut f = match fs::File::create(&target) {
            Ok(f) => f,
            Err(_) => continue,
        };
        if std::io::copy(&mut entry, &mut f).is_ok() {
            out.push(target.to_string_lossy().to_string());
        }
    }
    if out.is_empty() {
        return Err("no WAV in zip".into());
    }
    Ok(out)
}

/// Comando exposto ao frontend: escaneia uma pasta de exportacao.
pub fn scan(folder: &str) -> Result<ScanResult, String> {
    let mut dir = Path::new(folder);
    if !dir.exists() {
        return Err(format!("Folder not found: {}", folder));
    }
    // Arrastar um arquivo em vez da pasta: escaneia a pasta pai.
    if !dir.is_dir() {
        dir = dir
            .parent()
            .filter(|p| p.is_dir())
            .ok_or_else(|| format!("Path is not a folder: {}", folder))?;
    }

    let mut files = Vec::new();
    walk(dir, false, 0, &mut files);

    // Ordena: identificados primeiro (por categoria), indefinidos por ultimo.
    files.sort_by(|a, b| {
        let au = a.category == "undefined";
        let bu = b.category == "undefined";
        au.cmp(&bu)
            .then_with(|| a.category.cmp(&b.category))
            .then_with(|| a.filename.cmp(&b.filename))
    });

    let undefined_count = files.iter().filter(|f| f.category == "undefined").count();
    let has_extended_master = files.iter().any(|f| f.category == "extended_mix");

    Ok(ScanResult {
        folder: dir.to_string_lossy().to_string(),
        files,
        undefined_count,
        has_extended_master,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(name: &str) -> String {
        let n = name.to_lowercase();
        let ext = ext_of(name);
        classify(&n, &ext, false).category.to_string()
    }

    #[test]
    fn detecta_wavs_pela_nomenclatura() {
        // A ordem importa: mais especifico vence.
        assert_eq!(cat("Track - Extended Mix.wav"), "extended_mix");
        assert_eq!(cat("Track - Extended Mixdown.wav"), "extended_mixdown");
        assert_eq!(cat("Track - Extended Instrumental.wav"), "extended_instrumental");
        assert_eq!(
            cat("Track - Extended Instrumental Mixdown.wav"),
            "extended_instrumental_mixdown"
        );
        assert_eq!(cat("Track - Radio Mix.wav"), "radio_mix");
        assert_eq!(cat("Track - Radio Mixdown.wav"), "radio_mixdown");
        assert_eq!(cat("Track - Radio Instrumental.wav"), "radio_instrumental");
        assert_eq!(
            cat("Track - Radio Instrumental Mixdown.wav"),
            "radio_instrumental_mixdown"
        );
        // "Master" tambem conta como mix master.
        assert_eq!(cat("Track Master.wav"), "extended_mix");
        assert_eq!(cat("Track Radio Master.wav"), "radio_mix");
    }

    #[test]
    fn unmastered_vai_para_o_slot_do_mixdown() {
        // Nomes reais de uma track de 2026-09 (o produtor escreveu "unmastered"
        // em vez de "mixdown"). Como "unmastered" contem "master", antes desta
        // regra o mixdown Extended era carregado no campo `file` — o slot do
        // MASTER — e o comprador levava o ficheiro errado.
        assert_eq!(cat("quiet-rise-125-Cm-deep-house-extended-unmastered.wav"), "extended_mixdown");
        assert_eq!(cat("quiet-rise-125-Cm-deep-house-extended-instrumental-unmastered.wav"), "extended_instrumental_mixdown");
        assert_eq!(cat("quiet-rise-125-Cm-deep-house-radio-unmastered.wav"), "radio_mixdown");
        assert_eq!(cat("quiet-rise-125-Cm-deep-house-radio-instrumental-unmastered.wav"), "radio_instrumental_mixdown");

        let n = "track - extended unmastered.wav".to_string();
        let c = classify(&n, "wav", false);
        assert_eq!(c.upload_field, Some("xf_extended_mixdown"));
        assert!(!c.is_master, "um unmastered nunca pode ser marcado como master");

        // O master de verdade continua a ir para o campo `file`.
        assert_eq!(cat("Track - Extended Master.wav"), "extended_mix");
    }

    #[test]
    fn radio_instrumental_e_mixdown_sobem_no_upload() {
        // Ate 2026-08 esses 2 eram role "skip" / upload_field None (decisao
        // antiga do GPW). Agora sobem como o Extended equivalente.
        let n = "track - radio instrumental.wav".to_string();
        let c = classify(&n, "wav", false);
        assert_eq!(c.role, "instrumental");
        assert_eq!(c.upload_field, Some("xf_radio_instrumental"));

        let n2 = "track - radio instrumental mixdown.wav".to_string();
        let c2 = classify(&n2, "wav", false);
        assert_eq!(c2.role, "mixdown");
        assert_eq!(c2.upload_field, Some("xf_radio_instrumental_mixdown"));

        let n3 = "track - radio mixdown.wav".to_string();
        let c3 = classify(&n3, "wav", false);
        assert_eq!(c3.role, "mixdown");
        assert_eq!(c3.upload_field, Some("xf_radio_mixdown"));
    }

    #[test]
    fn master_extended_vai_para_campo_file() {
        let n = "extended mix.wav".to_string();
        let c = classify(&n, "wav", false);
        assert_eq!(c.upload_field, Some("file"));
        assert!(c.is_master);
    }

    #[test]
    fn outros_tipos() {
        assert_eq!(cat("cover.jpg"), "cover");
        assert_eq!(cat("artwork.png"), "cover");
        assert_eq!(cat("project.flp"), "project");
        assert_eq!(cat("session.als"), "project");
        assert_eq!(cat("melody.mid"), "midi");
        assert_eq!(cat("Stems.zip"), "stems");
        assert_eq!(cat("license.pdf"), "license");
        assert_eq!(cat("teaser.mp4"), "video");
        assert_eq!(cat("random.txt"), "undefined");
        assert_eq!(cat("notes"), "undefined");
    }

    #[test]
    fn stems_soltos_dentro_de_pasta_stems() {
        // arquivo sem extensao util, mas dentro de pasta "stems"
        let n = "kick".to_string();
        assert_eq!(classify(&n, "", true).category, "stems");
        // wav fora de pasta stems, sem keyword -> indefinido
        assert_eq!(cat("kick.wav"), "undefined");
    }

    #[test]
    fn wav_dentro_de_pasta_stems_nunca_e_master() {
        // "Lead Mix.wav" contem "mix" mas esta na pasta de stems: e stem.
        let c = classify("lead mix.wav", "wav", true);
        assert_eq!(c.category, "stems");
        assert!(!c.is_master);
        assert_eq!(classify("kick.wav", "wav", true).category, "stems");
    }

    #[test]
    fn qualquer_mix_nao_radio_vira_extended_master() {
        // Comportamento por design: keyword "mix" basta. Duplicados
        // (2+ arquivos no mesmo campo) sao tratados na UI (dedupe + aviso).
        assert_eq!(cat("Track (Club Mix).wav"), "extended_mix");
        assert_eq!(cat("Track Remix.wav"), "extended_mix");
    }

    #[test]
    fn zip_de_stems_feito_no_mac_ignora_os_appledouble() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("gpw_uploader_macjunk_test");
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("Stems.zip");
        let mut zw = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("Stems/Kick.wav", opts).unwrap();
        zw.write_all(b"fake wav").unwrap();
        zw.start_file("__MACOSX/Stems/._Kick.wav", opts).unwrap();
        zw.write_all(b"appledouble").unwrap();
        zw.finish().unwrap();

        let wavs = extract_wavs_from_zip(&zip_path.to_string_lossy()).unwrap();
        assert_eq!(wavs.len(), 1);
        assert!(wavs[0].ends_with("Kick.wav") && !wavs[0].contains("._"));
    }
}
