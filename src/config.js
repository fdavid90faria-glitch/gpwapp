// config.js (Fase 5) - memoria de preferencias do produtor + constantes
// compartilhadas (lista de generos, conexao Supabase).
//
// Persistido em localStorage (mesma estrategia do auth). O produtor preenche
// uma vez; a tela de revisao ja vem com tudo preenchido.

export const SUPABASE_URL = "https://dhtylvgufuqsjuoxfxst.supabase.co";
// Chave PUBLICA (publishable). A anon JWT antiga foi desligada em 2026-10-23
// (rotacao depois do vazamento no Railway).
export const SUPABASE_ANON_KEY = "sb_publishable_7J-btqnPZNeEVRadV-t5_w_XwQIHjxn";

// Lista de generos do site (public/upload.html, select #selGenre).
export const GENRES = [
  "Afro House", "Ambient", "Bass House", "Big Room", "Big Room Techno", "Breakbeat", "Breaks", "Chillout",
  "Dance", "Deep House", "Deep Tech", "Downtempo", "Drum and Bass", "Dubstep",
  "Electro House", "Funky House", "Future Bass", "Future House", "Grime",
  "Hard Dance", "Hard Techno", "Hardcore", "Hardstyle", "House", "Hyperpop",
  "Indie Dance", "Indie Tech", "Jackin House", "Latin Electronic", "LoFi", "Mainstage", "Melodic House", "Melodic Techno",
  "Minimal", "Minimal Tech", "Moombahton", "Neo Rave", "Nu Disco", "Organic", "Phonk", "Pop", "Progressive House",
  "Psy-Trance", "Slap House", "Speed Garage", "Synthwave", "Tech House", "Techno",
  "Trance", "Trap", "UK Garage",
];

const DEFAULTS_KEY = "gpwUploader.defaults";

const FALLBACK_DEFAULTS = {
  os: "Windows",
  daw: "FL Studio",
  daw_version: "",
  plugins: "",
  hardware: "",
  default_genre: "",
  default_project_for_sale: false,
  default_customization: false,
  default_customization_price: 50,
};

export function loadDefaults() {
  try {
    const raw = localStorage.getItem(DEFAULTS_KEY);
    if (raw) return { ...FALLBACK_DEFAULTS, ...JSON.parse(raw) };
  } catch (err) {
    console.warn("Failed to load defaults:", err);
  }
  return { ...FALLBACK_DEFAULTS };
}

export function saveDefaults(defaults) {
  try {
    localStorage.setItem(DEFAULTS_KEY, JSON.stringify(defaults));
  } catch (err) {
    console.warn("Failed to save defaults:", err);
  }
}
