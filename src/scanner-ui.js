// scanner-ui.js (Fase 2) - renderiza o resultado do scan_folder na UI.

// Campos obrigatorios pela UI do site (CONTRATO_UPLOAD.md). Usado para avisar
// o produtor se faltar algo essencial na pasta.
const REQUIRED = {
  extended_mix: "Extended Mix (master)",
  extended_mixdown: "Extended Mixdown",
  midi: "MIDI",
  stems: "Stems",
  project: "Project",
};

// Grupo Instrumental: so faz sentido numa track COM vocais. Pasta sem nenhum
// instrumental = track instrumental, e nao falta nada — em vez do aviso de
// "falta", o QC varre o master/mixdown a procura de voz (qc.js). Se o produtor
// enviou UM deles, a track tem vocais e o par tem de estar completo.
const INSTRUMENTAL_GROUP = {
  extended_instrumental: "Extended Instrumental",
  extended_instrumental_mixdown: "Extended Instrumental Mixdown",
};

// Grupo Radio (secao 3): cada um e opcional isolado, mas no site, se QUALQUER
// um for enviado, o grupo inteiro passa a obrigatorio. radio_mp3 fica de fora
// — e gerado pelo app a partir do radio_mix, nunca vem da pasta.
const RADIO_GROUP = {
  radio_mix: "Radio Mix",
  radio_mixdown: "Radio Mixdown",
  radio_instrumental: "Radio Instrumental Master",
  radio_instrumental_mixdown: "Radio Instrumental Mixdown",
};

const ROLE_ICON = {
  master: "★",
  mixdown: "▣",
  instrumental: "◐",
  mp3: "♪",
  support: "◆",
  image: "▦",
  skip: "–",
  undefined: "⚠",
};

function formatSize(bytes) {
  if (!bytes) return "";
  const mb = bytes / (1024 * 1024);
  if (mb >= 1) return `${mb.toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

function renderSummary(result, container) {
  container.innerHTML = "";
  const total = result.files.length;
  const ok = total - result.undefined_count;

  const stat = (n, label, mod) => {
    const box = el("div", `stat${mod ? " stat--" + mod : ""}`);
    box.appendChild(el("span", "stat__num", String(n)));
    box.appendChild(el("span", "stat__label", label));
    return box;
  };

  container.appendChild(stat(total, "files", null));
  container.appendChild(stat(ok, "identified", "ok"));
  if (result.undefined_count > 0) {
    container.appendChild(stat(result.undefined_count, "undefined", "warn"));
  }
}

function renderWarnings(result, container) {
  container.innerHTML = "";
  const present = new Set(result.files.map((f) => f.category));
  const missing = Object.entries(REQUIRED)
    .filter(([cat]) => !present.has(cat))
    .map(([, label]) => label);

  if (!result.has_extended_master) {
    const w = el(
      "div",
      "warn-line warn-line--error",
      "⚠ No Extended Mix (master) found — it's the main file of the upload."
    );
    container.appendChild(w);
  }

  if (missing.length) {
    const w = el(
      "div",
      "warn-line",
      "Missing (required by site): " + missing.join(", ")
    );
    container.appendChild(w);
  }

  // Instrumental: nenhum na pasta = track sem vocais (nao e falta). Com um so,
  // a track tem voz e o par tem de vir completo.
  const instrPresent = Object.keys(INSTRUMENTAL_GROUP).filter((cat) => present.has(cat));
  const hasInstrumental = [...present].some((c) => c.includes("instrumental"));
  if (!instrPresent.length) {
    container.appendChild(
      el(
        "div",
        "warn-line",
        "No instrumental files — treating this as an instrumental track (no vocals). The audio check below will flag possible vocals in the mix."
      )
    );
  } else if (instrPresent.length < Object.keys(INSTRUMENTAL_GROUP).length) {
    const instrMissing = Object.entries(INSTRUMENTAL_GROUP)
      .filter(([cat]) => !present.has(cat))
      .map(([, label]) => label);
    container.appendChild(
      el(
        "div",
        "warn-line warn-line--error",
        "Vocal track detected (an instrumental version was exported) — the site then requires the full pair. Missing: " + instrMissing.join(", ")
      )
    );
  }

  // Grupo Radio ativado: avisa o que falta pra completar o grupo (o site
  // bloqueia o submit se ficar incompleto). Numa track sem vocais o Radio
  // tambem nao leva versoes instrumentais.
  const radioGroup = Object.entries(RADIO_GROUP).filter(
    ([cat]) => hasInstrumental || !cat.includes("instrumental")
  );
  const radioMissing = radioGroup
    .filter(([cat]) => !present.has(cat))
    .map(([, label]) => label);
  if (radioMissing.length && radioMissing.length < radioGroup.length) {
    container.appendChild(
      el(
        "div",
        "warn-line warn-line--error",
        "Radio version detected — the whole Radio group becomes required on the site. Missing: " + radioMissing.join(", ")
      )
    );
  }

  // Stems SOLTAS (WAVs numa pasta Stems/, nao um .zip): o site aceita as stems
  // como UM ficheiro .zip. Aviso claro em vez da mensagem de colisao de slot.
  // (As stems extraidas de um zip vem com upload_field null e nao contam aqui.)
  const looseStems = result.files.filter(
    (f) => f.category === "stems" && f.ext === "wav" && f.upload_field === "xf_stems"
  );
  if (looseStems.length) {
    container.appendChild(
      el(
        "div",
        "warn-line warn-line--error",
        `⚠ Stems folder must be zipped — the site accepts the stems as a single .zip. Zip your Stems folder (${looseStems.length} WAV${looseStems.length === 1 ? "" : "s"}) and drop the .zip instead.`
      )
    );
  }

  // Dois arquivos no mesmo campo do upload (duas capas, dois WAVs com "mix"
  // no nome): so o primeiro sera enviado. As stems soltas ja foram avisadas
  // acima — nao repetir a colisao de slot por cada stem.
  const firstByField = new Map();
  for (const f of result.files) {
    if (!f.upload_field || f.upload_field === "xf_stems") continue;
    const first = firstByField.get(f.upload_field);
    if (!first) {
      firstByField.set(f.upload_field, f);
    } else {
      container.appendChild(
        el(
          "div",
          "warn-line",
          `⚠ "${f.filename}" maps to the same slot as "${first.filename}" (${first.label}) — only the first will be uploaded. Rename or remove one if that's wrong.`
        )
      );
    }
  }
}

// WAV solto (fora de uma pasta de stems) e o unico caso em que a deteccao pelo
// nome decide algo que o produtor pode querer corrigir: master vs mixdown vs
// instrumental. Cover, ZIP, PDF e MP3 saem do tipo do ficheiro, nao do nome.
const canPickSlot = (f) => f.ext === "wav" && f.category !== "stems";

// Dropdown para corrigir o slot. O app adivinha pelo nome; quando erra, era o
// produtor que tinha de renomear o ficheiro e voltar a arrastar a pasta — e
// so reparava se lesse a lista com atencao. `slots` vem do Rust (wav_slots),
// nunca escrito a mao aqui.
function slotPicker(f, slots, onChange) {
  const sel = document.createElement("select");
  sel.className = "tag tag--field slot-picker";
  sel.title = "Which slot this file is uploaded to";
  for (const s of slots) {
    const opt = document.createElement("option");
    opt.value = s.category;
    opt.textContent = s.label;
    if (s.category === f.category) opt.selected = true;
    sel.appendChild(opt);
  }
  // Nome fora do padrao: o app nao tem palpite nenhum. Tem de ficar visivel
  // que falta decidir — "Don't upload" ja e uma decisao, e nao foi tomada.
  if (!slots.some((s) => s.category === f.category)) {
    const opt = document.createElement("option");
    opt.value = f.category;
    opt.textContent = "Choose a slot…";
    opt.selected = true;
    sel.insertBefore(opt, sel.firstChild);
    sel.classList.add("slot-picker--undecided");
  }
  sel.addEventListener("change", () => onChange(f, sel.value));
  return sel;
}

function renderList(result, list, slots, onSlotChange) {
  list.innerHTML = "";
  for (const [i, f] of result.files.entries()) {
    const undef = f.category === "undefined";
    const li = el("li", `file-row${undef ? " file-row--undefined" : ""}`);
    li.id = `file-row-${i}`; // qc.js anexa o resultado da analise aqui

    const icon = el(
      "span",
      "file-row__icon",
      ROLE_ICON[f.role] || ROLE_ICON.undefined
    );

    const main = el("div", "file-row__main");
    main.appendChild(el("span", "file-row__label", f.label));
    main.appendChild(el("span", "file-row__name", f.filename));

    const meta = el("div", "file-row__meta");
    if (slots && slots.length && canPickSlot(f)) {
      meta.appendChild(slotPicker(f, slots, onSlotChange));
    } else if (f.upload_field) {
      meta.appendChild(el("span", "tag tag--field", f.upload_field));
    } else if (f.role === "skip") {
      meta.appendChild(el("span", "tag tag--skip", "not uploaded"));
    } else {
      meta.appendChild(el("span", "tag tag--undef", "categorize"));
    }
    if (f.size) meta.appendChild(el("span", "file-row__size", formatSize(f.size)));

    li.appendChild(icon);
    li.appendChild(main);
    li.appendChild(meta);
    list.appendChild(li);
  }
}

export function renderScan(result, els, slots, onSlotChange) {
  els.resultsPath.textContent = result.folder;
  renderSummary(result, els.resultsSummary);
  renderWarnings(result, els.resultsWarnings);
  renderList(result, els.resultsList, slots, onSlotChange);
}
