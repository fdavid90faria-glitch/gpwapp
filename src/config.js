// config.js (Fase 5) - memoria de preferencias do produtor + constantes
// compartilhadas (lista de generos, conexao Supabase).
//
// Persistido em localStorage (mesma estrategia do auth). O produtor preenche
// uma vez; a tela de revisao ja vem com tudo preenchido.

export const SUPABASE_URL = "https://dhtylvgufuqsjuoxfxst.supabase.co";
export const SUPABASE_ANON_KEY =
  "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6ImRodHlsdmd1ZnVxc2p1b3hmeHN0Iiwicm9sZSI6ImFub24iLCJpYXQiOjE3ODE1NTEwMzYsImV4cCI6MjA5NzEyNzAzNn0.ypGbYS5S-orm4OF61uUPQ9kGO28eYyYZx3nJ53BWF_o";

// Lista de generos do site (public/upload.html, select #selGenre).
export const GENRES = [
  "Afro House", "Ambient", "Bass House", "Big Room", "Breaks", "Chillout",
  "Dance", "Deep House", "Deep Tech", "Drum & Bass", "Dubstep",
  "Electro House", "Future Bass", "Future House", "Hardstyle", "House",
  "Indie Dance", "LoFi", "Mainstage", "Melodic Techno",
  "Minimal", "Nu Disco", "Phonk", "Progressive House",
  "Psy-Trance", "Slap House", "Synthwave", "Tech House", "Techno",
  "Trance", "Trap", "UK Garage",
];

const DEFAULTS_KEY = "gpwUploader.defaults";

const FALLBACK_DEFAULTS = {
  os: "Windows",
  daw: "FL Studio",
  daw_version: "",
  plugins: "",
  hardware: "",
  default_price: 300,
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
  localStorage.setItem(DEFAULTS_KEY, JSON.stringify(defaults));
}
