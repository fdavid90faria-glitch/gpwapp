// qc.js - controle de qualidade dos arquivos de audio contra o padrao GPW.
// Regras portadas do GPW ANALYZER (validate + collectIssues); as metricas vem
// do comando Rust `qc_analyze` (streaming, sem ffmpeg). Os resultados entram
// nas fileiras de arquivo ja existentes (nenhuma tela nova) — erros em
// vermelho, avisos em amarelo, e as checagens entre arquivos no bloco de
// warnings do scan.
//
// ponytail: sem QC de MP3 (o app gera os MP3s ele mesmo a 320k), sem MIDI e
// sem soma-de-stems-vs-mixdown (pesados; o A&R QC do site cobre) — adicionar
// se arquivos errados desses tipos começarem a passar.

const { invoke } = window.__TAURI__.core;

const fmtDb = (v) => (v == null ? "—" : v.toFixed(1));
const fmtTime = (s) => {
  if (s == null || !isFinite(s)) return "0:00";
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
};
const fmtFormat = (a) =>
  `WAV ${a.bit_depth}-bit${a.is_float ? " float" : ""} · ${(a.sample_rate / 1000).toFixed(1).replace(".0", "")} kHz`;

// Papel do arquivo nas regras de QC. Mais fino que o role do scanner: um WAV
// "Instrumental Master" sobe no slot de instrumental, mas e um MASTER para as
// regras de peak (mesma distincao do ANALYZER).
function qcRole(f) {
  if (f.ext !== "wav") return null; // so WAV tem regra de formato/peak
  if (f.category === "stems") return "stems";
  // Masters (versoes masterizadas, peak -0.3..0 dB): Extended Mix, o Extended
  // Instrumental (Mix) e o Radio Mix. O Extended Instrumental e a versao master
  // instrumental — NAO um mixdown (esse e o extended_instrumental_mixdown).
  if (
    f.category === "extended_mix" ||
    f.category === "extended_instrumental" ||
    f.category === "radio_mix"
  )
    return "master";
  const n = f.filename.toLowerCase();
  if (n.includes("instrumental") && n.includes("master")) return "master";
  // Mixdowns (headroom p/ masterizacao, peak <= -3 dB): Extended Mixdown e o
  // Extended Instrumental Mixdown. Stems ja tratados acima.
  if (
    f.category === "extended_mixdown" ||
    f.category === "extended_instrumental_mixdown"
  )
    return "mixdown";
  return "silence"; // radio_* (fora do upload) e indefinidos: so acusa vazio
}

// Padrao GPW (secao 1 da arquitetura do ANALYZER):
//   master  -> WAV 24-bit, peak -0.3..0 dB, LUFS short-term <= -3
//   mixdown/stems -> WAV 24-bit, peak <= -3 dB
//   cauda de silencio > 3s = bug de export (nao se aplica a stems)
function validate(role, a) {
  const reasons = [];
  const warns = [];
  const silence = a.is_silent ? "No audio — file is empty" : null;
  if (role === "silence")
    return silence ? { level: "fail", reasons: [silence] } : { level: "ok", reasons: [] };

  if (silence) reasons.push(silence);

  const isWav24 = a.bit_depth === 24 && !a.is_float;
  if (role !== "stems" && a.trailing_silence_secs > 3)
    reasons.push(`${a.trailing_silence_secs.toFixed(1)}s of silence at the end (export bug?)`);

  // 1 casa decimal para os limites serem inclusivos (-0.3000x conta como -0.3)
  const p = a.sample_peak_db == null ? null : Math.round(a.sample_peak_db * 10) / 10;
  if (role === "master") {
    if (!isWav24) reasons.push(`Format: expected WAV 24-bit, found ${fmtFormat(a)}`);
    if (p == null || p > 0 || p < -0.3)
      reasons.push(`Peak ${fmtDb(p)} dB outside the -0.3 to 0 dB range`);
    if (a.lufs_short_max != null && a.lufs_short_max > -3)
      reasons.push(`LUFS Short-Term ${fmtDb(a.lufs_short_max)} above the max -3`);
  } else {
    if (!isWav24) reasons.push(`Format: expected WAV 24-bit, found ${fmtFormat(a)}`);
    if (p != null && p > -3 && !a.is_silent)
      reasons.push(`Peak ${fmtDb(p)} dB above the max -3 dB`);
  }

  if (a.vocal_confidence != null && a.vocal_confidence >= 0.6)
    warns.push(`Possible vocal detected (${Math.round(a.vocal_confidence * 100)}% confidence) — check by ear`);

  if (reasons.length) return { level: "fail", reasons: reasons.concat(warns) };
  if (warns.length) return { level: "warn", reasons: warns };
  return { level: "ok", reasons: [] };
}

// Checagens entre arquivos: (1) duracao das versoes Extended vs a Extended
// Master; (2) duracao das STEMS comparadas ENTRE SI (uma stem fora do padrao
// das outras); (3) volume Radio vs Extended (LUFS +-1 dB).
function collectCrossIssues(items) {
  const issues = [];
  const ref = items.find((x) => x.file.category === "extended_mix" && x.analysis);
  // (1) Versoes Extended (NAO stems) vs a Extended Master.
  if (ref) {
    for (const x of items) {
      if (x === ref || !x.analysis) continue;
      const c = x.file.category;
      if (c === "stems" || !c.startsWith("extended")) continue;
      const diff = Math.abs(x.analysis.duration - ref.analysis.duration);
      if (diff > 1)
        issues.push(
          `"${x.file.filename}": duration ${fmtTime(x.analysis.duration)} differs from the Extended Master (${fmtTime(ref.analysis.duration)})`
        );
    }
  }
  // (2) Stems entre si: a referencia e a mediana das duracoes das stems; uma
  // stem que se afaste > 1 s do resto e apontada (uma stem com tamanho
  // diferente das outras).
  const stems = items.filter((x) => x.file.category === "stems" && x.analysis);
  if (stems.length >= 2) {
    const durs = stems.map((x) => x.analysis.duration).sort((a, b) => a - b);
    const median = durs[Math.floor(durs.length / 2)];
    for (const x of stems) {
      if (Math.abs(x.analysis.duration - median) > 1)
        issues.push(
          `"${x.file.filename}" (stem): duration ${fmtTime(x.analysis.duration)} differs from the other stems (${fmtTime(median)})`
        );
    }
  }
  const rad = items.find((x) => x.file.category === "radio_mix" && x.analysis);
  if (ref && rad && ref.analysis.lufs_integrated != null && rad.analysis.lufs_integrated != null) {
    const d = rad.analysis.lufs_integrated - ref.analysis.lufs_integrated;
    if (Math.abs(d) > 1)
      issues.push(
        `Radio Master is ${Math.abs(d).toFixed(1)} dB ${d > 0 ? "LOUDER" : "QUIETER"} (LUFS) than the Extended Master`
      );
  }
  return issues;
}

function appendQcLines(row, verdict) {
  if (!row) return;
  const main = row.querySelector(".file-row__main");
  if (!main) return;
  row.querySelectorAll(".qc-line").forEach((n) => n.remove());
  for (const r of verdict.reasons) {
    const div = document.createElement("div");
    div.className = `qc-line qc-line--${verdict.level === "fail" ? "fail" : "warn"}`;
    div.textContent = `${verdict.level === "fail" ? "✗" : "⚠"} ${r}`;
    main.appendChild(div);
  }
}

// Roda o QC sobre o resultado do scan e vai pintando as fileiras. Nao bloqueia
// o upload — so indica. `isCurrent()` descarta resultados de um scan antigo.
export async function runQc(scan, isCurrent, warningsEl) {
  const items = scan.files
    .map((file, index) => ({ file, index, role: qcRole(file), analysis: null, verdict: null }))
    .filter((x) => x.role);
  if (!items.length) return;

  // pool de 2 (cada analise ja usa uma thread no Rust; disco e o gargalo)
  const queue = [...items];
  const worker = async () => {
    let x;
    while ((x = queue.shift())) {
      try {
        x.analysis = await invoke("qc_analyze", { path: x.file.path, category: x.file.category });
        x.verdict = validate(x.role, x.analysis);
      } catch (e) {
        x.verdict = { level: "fail", reasons: [`Could not analyze: ${e}`] };
      }
      if (!isCurrent()) return;
      appendQcLines(document.getElementById(`file-row-${x.index}`), x.verdict);
    }
  };
  await Promise.all([worker(), worker()]);
  if (!isCurrent()) return;

  const cross = collectCrossIssues(items);

  // Soma das stems: a SOMA de todos os stems nao pode passar de -3 dB de peak
  // (mesma regra do ANALYZER). Corre em Rust; falha silenciosa se der erro.
  const stemPaths = items.filter((x) => x.role === "stems" && x.analysis).map((x) => x.file.path);
  if (stemPaths.length >= 2) {
    try {
      const sum = await invoke("qc_stems_sum", { paths: stemPaths });
      if (!isCurrent()) return;
      const sp = sum && sum.sample_peak_db == null ? null : Math.round(sum.sample_peak_db * 10) / 10;
      if (sp != null && sp > -3)
        cross.push(`Summed stems peak ${fmtDb(sp)} dB above the max -3 dB (the ${sum.count} stems together clip)`);
    } catch (e) { /* soma indisponivel — nao bloqueia */ }
  }
  const failed = items.filter((x) => x.verdict && x.verdict.level === "fail").length;
  const line = document.createElement("div");
  if (cross.length || failed) {
    line.className = "warn-line warn-line--error";
    line.textContent =
      `✗ Audio check: ${failed ? `${failed} file(s) out of the GPW standard` : ""}` +
      (failed && cross.length ? " · " : "") +
      cross.map((c) => `⚠ ${c}`).join(" · ");
  } else {
    line.className = "warn-line warn-line--ok";
    line.textContent = `✓ Audio check passed — ${items.length} file(s) within the GPW standard`;
  }
  warningsEl.appendChild(line);
}
