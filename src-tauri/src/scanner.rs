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
    /// mtime em ms (0 se indisponivel): o Continue confere se o ficheiro mudou
    /// desde o scan (commands::stat_files).
    pub modified: u64,
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

#[derive(Serialize, Clone)]
pub struct Classification {
    category: &'static str,
    label: &'static str,
    role: &'static str,
    upload_field: Option<&'static str>,
    is_master: bool,
}

/// Os slots de WAV que o produtor pode escolher A MAO na UI quando o app
/// adivinha mal pelo nome. A deteccao por nomenclatura e um palpite; o slot e o
/// que o comprador recebe, por isso tem de haver forma de o corrigir antes de
/// enviar.
///
/// Materializa o que classify_wav() produz — `slots_batem_com_classify_wav`
/// trava se as duas listas divergirem, para a UI nunca oferecer um slot que o
/// scanner nao conhece (ou esquecer um que ele conhece).
pub const WAV_SLOTS: &[Classification] = &[
    Classification { category: "extended_mix", label: "Extended Mix (master)", role: "master", upload_field: Some("file"), is_master: true },
    Classification { category: "extended_mixdown", label: "Extended Mixdown", role: "mixdown", upload_field: Some("xf_extended_mixdown"), is_master: false },
    Classification { category: "extended_instrumental", label: "Extended Instrumental", role: "instrumental", upload_field: Some("xf_extended_instrumental"), is_master: false },
    Classification { category: "extended_instrumental_mixdown", label: "Extended Instrumental Mixdown", role: "mixdown", upload_field: Some("xf_extended_instrumental_mixdown"), is_master: false },
    Classification { category: "radio_mix", label: "Radio Mix (master)", role: "master", upload_field: Some("xf_radio_mix"), is_master: true },
    Classification { category: "radio_mixdown", label: "Radio Mixdown", role: "mixdown", upload_field: Some("xf_radio_mixdown"), is_master: false },
    Classification { category: "radio_instrumental", label: "Radio Instrumental Master", role: "instrumental", upload_field: Some("xf_radio_instrumental"), is_master: false },
    Classification { category: "radio_instrumental_mixdown", label: "Radio Instrumental Mixdown", role: "mixdown", upload_field: Some("xf_radio_instrumental_mixdown"), is_master: false },
    // "nao enviar": para tirar do upload um WAV que nao pertence ao pack.
    // Categoria PROPRIA, nunca "undefined": "o app nao sabe o que isto e" e
    // "o produtor decidiu nao enviar" sao coisas diferentes, e na UI a segunda
    // nao pode aparecer como se ja tivesse sido decidida. classify_wav nunca
    // produz esta categoria — so a escolha manual.
    Classification { category: "not_uploaded", label: "Don't upload", role: "skip", upload_field: None, is_master: false },
];

const UNDEFINED: Classification = Classification {
    category: "undefined",
    label: "Unidentified",
    role: "undefined",
    upload_field: None,
    is_master: false,
};

/// Ficheiro solto numa pasta de stems: so entra no QC; o que sobe e o .zip.
const LOOSE_STEM: Classification = Classification {
    category: "stems",
    label: "Stem (loose — zip the folder)",
    role: "support",
    upload_field: None,
    is_master: false,
};

/// Alguma palavra do nome (partido em tudo o que nao e letra/digito) cumpre
/// `pred`? Por substring, o titulo da track caia nas palavras-chave: "mid" em
/// Midnight/Pyramid (projeto ia para o MIDI), "stem" em System, "radio" em
/// Radioactive. Limite: nomes colados ("TrackStems.zip") deixam de casar e caem
/// em indefinido, que a UI mostra.
fn has_word(name_lower: &str, pred: impl Fn(&str) -> bool) -> bool {
    name_lower.split(|c: char| !c.is_alphanumeric()).any(pred)
}

/// Nome de pasta/zip de stems: uma palavra que comeca por "stem" (Stems, Stem).
fn is_stems_name(name_lower: &str) -> bool {
    has_word(name_lower, |w| w.starts_with("stem"))
}

/// Classifica um WAV de audio pela nomenclatura (secao 3).
fn classify_wav(name_lower: &str) -> Classification {
    let radio = has_word(name_lower, |w| w == "radio");
    let words: Vec<&str> = name_lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let pair = |a: &str, b: &[&str]| words.windows(2).any(|p| p[0] == a && b.contains(&p[1]));
    let instrumental = name_lower.contains("instrumental") || words.iter().any(|w| *w == "inst" || *w == "instr");
    // "Unmastered" e sinonimo de mixdown — e a palavra que a copy do site usa
    // ("the unmastered mixdown"), por isso e a que muitos produtores poem no
    // nome. Como "unmastered" CONTEM "master", sem isto um
    // "...-extended-unmastered.wav" caia no slot do Extended Mix MASTER (o
    // campo `file`): o mixdown era enviado como se fosse o master. O mixdown e
    // sempre testado antes do master_kw nos dois ramos, por isso basta aqui.
    // Os outros nomes comuns do mixdown, todos com "master"/"mix" dentro:
    // Premaster, Pre-Master, Non-Mastered, Not Mastered, Mix Down. Por palavras
    // e nao por substring: "Techno Master" tem "no master" la dentro.
    const MASTER: &[&str] = &["master", "mastered"];
    let mixdown = name_lower.contains("mixdown")
        || name_lower.contains("unmaster")
        || name_lower.contains("premaster")
        || name_lower.contains("nonmaster")
        || pair("mix", &["down"])
        || pair("pre", MASTER)
        || pair("non", MASTER)
        || pair("not", MASTER);
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
        // Sem upload_field: o site so aceita as stems num .zip, e um WAV solto
        // no xf_stems tirava o lugar ao zip valido. A UI pede para zipar.
        "wav" if in_stems_dir => LOOSE_STEM,
        "wav" => classify_wav(name_lower),

        // MP3 da pasta nunca sobe: o app gera os dois MP3 a partir dos masters.
        // Antes qualquer .mp3 (um demo, o instrumental) ocupava o slot do
        // Extended Mix MP3 e ainda cancelava a conversao do master.
        "mp3" => Classification { category: "mp3_ignored", label: "MP3 (the app converts the masters)", role: "skip", upload_field: None, is_master: false },

        // O site (lib/upload-exts.js allowedExts) so aceita MIDI e projeto em
        // .zip, e nao tem slot de video nem de licenca no formulario. Antes
        // estes subiam e eram recusados no fim, ja com o rascunho criado.
        // Ficam por identificar, com o rotulo a dizer o que fazer.
        "mid" | "midi" => Classification { label: "MIDI file — zip your MIDI files to upload them", ..UNDEFINED },

        "flp" | "als" | "alp" | "logic" | "logicx" | "cpr" | "ableton" | "ptx" | "song" | "bwproject" =>
            Classification { label: "Project file — zip the project to upload it", ..UNDEFINED },

        "jpg" | "jpeg" | "png" | "webp" =>
            Classification { category: "cover", label: "Cover", role: "image", upload_field: Some("cover"), is_master: false },

        "pdf" => Classification { label: "PDF — not uploaded by the app", ..UNDEFINED },

        "mp4" => Classification { label: "Video — not uploaded by the app", ..UNDEFINED },

        "zip" => {
            if is_stems_name(name_lower) || in_stems_dir {
                Classification { category: "stems", label: "Stems (ZIP)", role: "support", upload_field: Some("xf_stems"), is_master: false }
            } else if has_word(name_lower, |w| w == "midi" || w == "mid") {
                Classification { category: "midi", label: "MIDI (ZIP)", role: "support", upload_field: Some("xf_midi"), is_master: false }
            } else if has_word(name_lower, |w| w == "master") && has_word(name_lower, |w| w.starts_with("proj")) {
                // Projeto de masterizacao: e o add-on "Master Project" do site,
                // que o rascunho do app nao liga. Nao pode ir no slot do projeto.
                Classification { label: "Master project — upload it on the site", ..UNDEFINED }
            } else if has_word(name_lower, |w| w.starts_with("proj")) {
                Classification { category: "project", label: "Project (ZIP)", role: "support", upload_field: Some("xf_project"), is_master: false }
            } else {
                UNDEFINED
            }
        }

        _ => {
            // Sem extensao util: pode ser stem solto dentro de pasta "stems".
            if in_stems_dir {
                LOOSE_STEM
            } else {
                UNDEFINED
            }
        }
    }
}

pub fn mtime_ms(m: &fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn stems_tmp_root() -> std::path::PathBuf {
    std::env::temp_dir().join("gpw_uploader_stems")
}

/// Apaga as extracoes de stems anteriores (o comando scan_folder chama antes
/// de cada scan): so a ronda atual precisa dos WAVs, e antes cada pack deixava
/// ~1x o tamanho do zip no %TEMP% para sempre.
/// ponytail: apaga tambem a extracao de uma 2.a instancia do app a correr ao
/// mesmo tempo (o QC dela falha); raro, single-instance resolve se doer.
pub fn clear_stems_tmp() {
    let _ = fs::remove_dir_all(stems_tmp_root());
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
            let child_in_stems = in_stems_dir || is_stems_name(&name_lower);
            walk(&path, child_in_stems, depth + 1, out);
            continue;
        }

        // Ignora arquivos ocultos.
        if name_lower.starts_with('.') {
            continue;
        }

        let ext = ext_of(&file_name);
        // Nome que o Windows guarda mas nao e Unicode valido (surrogate solto):
        // o caminho "lossy" nao abria e falhava tarde, no QC e no upload.
        let Some(path_s) = path.to_str().map(String::from) else {
            out.push(ScannedFile {
                filename: file_name.clone(),
                path: String::new(),
                ext,
                size: 0,
                modified: 0,
                category: UNDEFINED.category.into(),
                label: "Rename this file (invalid characters in the name)".into(),
                role: UNDEFINED.role.into(),
                upload_field: None,
                is_master: false,
            });
            continue;
        };
        // A capa so vem da raiz da pasta: uma imagem qualquer numa subpasta
        // (screenshot, artwork antigo) ganhava a capa por ordem do nome.
        let c = if depth > 0 && matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp") {
            UNDEFINED
        } else {
            classify(&name_lower, &ext, in_stems_dir)
        };
        let md = entry.metadata().ok();

        out.push(ScannedFile {
            filename: file_name.clone(),
            path: path_s.clone(),
            ext: ext.clone(),
            size: md.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: md.as_ref().map(mtime_ms).unwrap_or(0),
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
        if ext == "zip" && (is_stems_name(&name_lower) || in_stems_dir) {
            if let Ok(wavs) = extract_wavs_from_zip(&path_s) {
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
                        modified: 0, // copia temporaria, nao sobe
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

    // Pasta propria por extracao: dois "Stems.zip" em pastas diferentes (ou um
    // scan antigo ainda a correr) partilhavam a mesma e apagavam-se um ao outro.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dest = stems_tmp_root().join(format!("{}-{}-{}", std::process::id(), n, name));
    fs::create_dir_all(&dest).map_err(|e| format!("temp dir: {}", e))?;

    let mut out = Vec::new();
    let mut used = std::collections::HashSet::new();
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ename = entry.name().to_string();
        if !ename.to_lowercase().ends_with(".wav") || is_mac_junk(&ename) {
            continue;
        }
        // Nomes validos no macOS mas nao no Windows (" ? * < > | :) eram
        // descartados em silencio; o ':' ("C:x.wav") ainda saia da pasta temp.
        let base: String = ename
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&ename)
            .chars()
            .map(|c| if c < ' ' || "<>:\"|?*".contains(c) { '_' } else { c })
            .collect();
        // Mesmo nome em subpastas diferentes ("Drums/FX.wav", "Synths/FX.wav"):
        // o 2.o sobrescrevia o 1.o e a soma contava um deles duas vezes.
        let base = if used.insert(base.to_lowercase()) { base } else { format!("{}_{}", i, base) };
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
    let dir = Path::new(folder);
    if !dir.exists() {
        return Err(format!("Folder not found: {}", folder));
    }
    // Um ficheiro arrastado escaneava a pasta-mae inteira, e os ficheiros de
    // outras tracks que la estivessem disputavam os slots.
    if !dir.is_dir() {
        return Err("Drop the track's export folder, not a single file.".into());
    }

    let mut files = Vec::new();
    walk(dir, false, 0, &mut files);

    // Dois WAVs no mesmo slot: o desempate era a ordem do nome ("Track
    // Extended Mix (v2).wav" ganhava ou perdia ao acaso). Ficam os dois por
    // decidir e o produtor escolhe no dropdown; sem master o Continue espera.
    let mut per_field: std::collections::HashMap<String, usize> = Default::default();
    for f in files.iter().filter(|f| f.ext == "wav" && f.category != "stems") {
        if let Some(field) = &f.upload_field {
            *per_field.entry(field.clone()).or_default() += 1;
        }
    }
    for f in files.iter_mut() {
        let dup = f.ext == "wav"
            && f.category != "stems"
            && f.upload_field.as_ref().is_some_and(|u| per_field[u] > 1);
        if dup {
            f.label = format!("Two files look like {} — choose a slot", f.label);
            f.category = UNDEFINED.category.into();
            f.role = UNDEFINED.role.into();
            f.upload_field = None;
            f.is_master = false;
        }
    }

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
    fn slots_batem_com_classify_wav() {
        // Nome canonico de cada slot -> a mesma classificacao que a UI oferece.
        // Se classify_wav mudar de label/role/upload_field e WAV_SLOTS ficar
        // para tras, o produtor escolhia um slot a mao e o ficheiro ia para
        // outro sitio. Aqui isso parte.
        let canonico = [
            ("track - extended mix.wav", "extended_mix"),
            ("track - extended mixdown.wav", "extended_mixdown"),
            ("track - extended instrumental.wav", "extended_instrumental"),
            ("track - extended instrumental mixdown.wav", "extended_instrumental_mixdown"),
            ("track - radio mix.wav", "radio_mix"),
            ("track - radio mixdown.wav", "radio_mixdown"),
            ("track - radio instrumental.wav", "radio_instrumental"),
            ("track - radio instrumental mixdown.wav", "radio_instrumental_mixdown"),
        ];
        for (nome, cat) in canonico {
            let c = classify_wav(nome);
            assert_eq!(c.category, cat, "{}", nome);
            let slot = WAV_SLOTS
                .iter()
                .find(|s| s.category == cat)
                .unwrap_or_else(|| panic!("WAV_SLOTS nao tem {}", cat));
            assert_eq!(slot.label, c.label, "label de {}", cat);
            assert_eq!(slot.role, c.role, "role de {}", cat);
            assert_eq!(slot.upload_field, c.upload_field, "upload_field de {}", cat);
            assert_eq!(slot.is_master, c.is_master, "is_master de {}", cat);
        }
        // e nenhum slot da UI existe sem o scanner o saber produzir
        let cats: Vec<&str> = canonico.iter().map(|(_, c)| *c).collect();
        for s in WAV_SLOTS.iter().filter(|s| s.category != "not_uploaded") {
            assert!(cats.contains(&s.category), "slot {} nao sai de classify_wav", s.category);
        }
        // "Don't upload" nao envia nada e e SO escolha manual: se o scanner
        // passasse a produzi-la, um ficheiro por identificar aparecia na UI
        // como se o produtor ja tivesse decidido nao o enviar.
        let none = WAV_SLOTS.iter().find(|s| s.category == "not_uploaded").unwrap();
        assert_eq!(none.upload_field, None);
        assert_ne!(classify_wav("qualquer coisa.wav").category, "not_uploaded");
        assert_eq!(classify_wav("qualquer coisa.wav").category, "undefined");
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
        assert_eq!(cat("Track - Project.zip"), "project");
        assert_eq!(cat("Track - MIDI.zip"), "midi");
        assert_eq!(cat("Stems.zip"), "stems");
        assert_eq!(cat("random.txt"), "undefined");
        assert_eq!(cat("notes"), "undefined");
    }

    #[test]
    fn so_sobe_o_que_o_site_aceita() {
        // lib/upload-exts.js no site: MIDI e projeto so em .zip; sem slot de
        // video nem de licenca no formulario. Recusados la, ja com rascunho.
        for n in ["project.flp", "session.als", "melody.mid", "license.pdf", "teaser.mp4", "Track Master Project.zip"] {
            let c = classify(&n.to_lowercase(), &ext_of(n), false);
            assert_eq!(c.category, "undefined", "{}", n);
            assert_eq!(c.upload_field, None, "{}", n);
        }
        // Stems soltas (e lixo na pasta Stems/) entram no QC mas nao sobem:
        // tiravam o lugar ao .zip no xf_stems.
        for (n, ext) in [("kick.wav", "wav"), ("desktop.ini", "ini"), ("kick.asd", "asd")] {
            let c = classify(n, ext, true);
            assert_eq!(c.category, "stems", "{}", n);
            assert_eq!(c.upload_field, None, "{}", n);
        }
        assert_eq!(classify("drums.zip", "zip", true).upload_field, Some("xf_stems"));
    }

    #[test]
    fn sinonimos_de_mixdown_e_instrumental() {
        for n in [
            "Track - Extended Premaster.wav",
            "Track - Extended Pre-Master.wav",
            "Track - Extended Mix Down.wav",
            "Track - Extended Non-Mastered.wav",
            "Track - Extended Not Mastered.wav",
        ] {
            assert_eq!(cat(n), "extended_mixdown", "{}", n);
        }
        assert_eq!(cat("Track - Radio Premaster.wav"), "radio_mixdown");
        assert_eq!(cat("Track - Extended Instr.wav"), "extended_instrumental");
        assert_eq!(cat("Track - Extended Inst Premaster.wav"), "extended_instrumental_mixdown");
        // Por palavras: "Techno Master" contem "no master", "Instinct" contem "inst".
        assert_eq!(cat("Techno Master.wav"), "extended_mix");
        assert_eq!(cat("Instinct - Extended Mix.wav"), "extended_mix");
    }

    #[test]
    fn scan_recusa_ficheiro_e_empata_wavs_no_mesmo_slot() {
        let dir = std::env::temp_dir().join(format!("gpw_scan_dup_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for n in ["Track - Extended Mix.wav", "Track - Extended Mix (v2).wav", "Track - Extended Mixdown.wav"] {
            fs::write(dir.join(n), b"x").unwrap();
        }
        assert!(scan(&dir.join("Track - Extended Mix.wav").to_string_lossy()).is_err());

        let r = scan(&dir.to_string_lossy()).unwrap();
        assert!(!r.has_extended_master, "dois candidatos a master: nenhum e escolhido sozinho");
        assert_eq!(r.undefined_count, 2);
        let mixdown = r.files.iter().find(|f| f.filename == "Track - Extended Mixdown.wav").unwrap();
        assert_eq!(mixdown.upload_field.as_deref(), Some("xf_extended_mixdown"));
        assert!(r.files.iter().all(|f| f.modified > 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mp3_da_pasta_nunca_sobe() {
        // O app gera os MP3 dos masters; um MP3 solto tomava o slot do Extended.
        for n in ["Track - Extended Instrumental.mp3", "demo.mp3", "Track - Radio Mix.mp3"] {
            let c = classify(&n.to_lowercase(), "mp3", false);
            assert_eq!(c.upload_field, None, "{}", n);
            assert_ne!(c.category, "undefined", "{} nao e 'por identificar', e ignorado", n);
        }
    }

    #[test]
    fn titulo_da_track_nao_dispara_palavras_chave() {
        // Por substring: "mid" em Midnight, "stem" em System, "radio" em Radioactive.
        assert_eq!(cat("Midnight Drive - Project.zip"), "project");
        assert_eq!(cat("Pyramid - Project.zip"), "project");
        assert_eq!(cat("Midnight Drive - MIDI.zip"), "midi");
        assert_eq!(cat("Sound System - MIDI.zip"), "midi");
        assert_eq!(cat("Sound System - Stems.zip"), "stems");
        assert_eq!(cat("Radioactive - Extended Mix.wav"), "extended_mix");
        assert_eq!(cat("Radioactive - Radio Mix.wav"), "radio_mix");
        assert!(!is_stems_name("system"));
        assert!(!is_stems_name("ecosystem"));
        assert!(is_stems_name("stems"));
        assert!(is_stems_name("track - stems (wav)"));
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
    fn extracao_nao_perde_nem_escapa_stems() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("gpw_zip_names_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("Stems.zip");
        let mut zw = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (n, body) in [("Drums/FX.wav", "a"), ("Synths/FX.wav", "b"), ("Lead?.wav", "c"), ("C:Bass.wav", "d")] {
            zw.start_file(n, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        }
        zw.finish().unwrap();

        let wavs = extract_wavs_from_zip(&zip_path.to_string_lossy()).unwrap();
        assert_eq!(wavs.len(), 4, "{:?}", wavs);
        let unique: std::collections::HashSet<_> = wavs.iter().collect();
        assert_eq!(unique.len(), 4, "dois stems no mesmo ficheiro: {:?}", wavs);
        let root = stems_tmp_root();
        assert!(wavs.iter().all(|w| Path::new(w).starts_with(&root)), "fora da pasta temp: {:?}", wavs);
        let bodies: Vec<String> = wavs.iter().map(|w| fs::read_to_string(w).unwrap()).collect();
        for b in ["a", "b", "c", "d"] {
            assert!(bodies.iter().any(|x| x == b), "stem {} perdido", b);
        }
        let _ = fs::remove_dir_all(&dir);
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
