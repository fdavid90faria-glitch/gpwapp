# Contrato do upload — `/api/tracks/upload`

Lido de `ghost-producer-world/public/upload.html` (autoritativo). O app deve
replicar exatamente este `multipart/form-data`, com header
`Authorization: Bearer <access_token>`.

## Campos de texto (sempre)

| Campo | Origem no form | Observação |
|---|---|---|
| `file` | **Extended Mix master WAV** | campo principal (não `xf_`) |
| `title` | `upTitle` | nome da track |
| `genre` | `selectedGenres[0]` | primeiro gênero |
| `bpm` | `upBpm` | |
| `music_key` | `upKey` | |
| `cover` | `upCover` (file) | opcional, enviado como `cover` |
| `price_eur` | `upPrice` | |
| `description` | `upShortInfo` | = short_info |
| `daw` | `daw + ' ' + version` | ex: "FL Studio 21.2" |
| `has_project_file` | `chkProject` | "true"/"false" |
| `metadata` | `JSON.stringify(metadata)` | objeto abaixo |

### Objeto `metadata` (JSON)

```json
{
  "genres": ["..."],
  "tags": ["..."],
  "short_info": "...",
  "process_note": "...",
  "production_type": "original|semi|hybrid",
  "samples": [],
  "vocal_type": "...",
  "vocal_provider": "...",
  "vocal_link": "...",
  "os": "...",
  "daw": "...",
  "daw_version": "...",
  "plugins": "...",
  "hardware": "...",
  "offers_enabled": false,
  "offers_unlock_days": "",
  "offers_max_discount": ""
}
```

## Campos de arquivo `xf_<data-fkey>` (todos opcionais no envio; alguns são obrigatórios pela UI)

| Arquivo | Campo multipart | `data-fkey` | Req. UI |
|---|---|---|---|
| Extended Mix (master WAV) | `file` | — (campo principal) | ✅ |
| Extended Mixdown | `xf_extended_mixdown` | `extended_mixdown` | ✅ |
| Extended Mix MP3 | `xf_extended_mp3` | `extended_mp3` | ✅ |
| Extended Instrumental | `xf_extended_instrumental` | `extended_instrumental` | ✅ |
| Extended Instrumental Mixdown | `xf_extended_instrumental_mixdown` | `extended_instrumental_mixdown` | ✅ |
| Radio Mix | `xf_radio_mix` | `radio_mix` | — |
| Radio Mixdown | `xf_radio_mixdown` | `radio_mixdown` | — |
| Radio Mix MP3 | `xf_radio_mp3` | `radio_mp3` | — |
| Radio Instrumental | `xf_radio_instrumental` | `radio_instrumental` | — |
| Radio Instrumental Mixdown | `xf_radio_instrumental_mixdown` | `radio_instrumental_mixdown` | — |
| MIDI (ZIP) | `xf_midi` | `midi` | ✅ |
| Stems (ZIP) | `xf_stems` | `stems` | ✅ |
| Project (ZIP) | `xf_project` | `project` | ✅ |
| License (PDF) | `xf_license` | `license` | — |
| Video (MP4) | `xf_video` | `video` | — |
| Cover | `cover` | — (id `upCover`) | — |

## Correções vs. ARQUITETURA_GPWAPP_v2.md

1. Master vai como `file` (não `upFileExtended`/`xf_`). Chaves radio reais:
   `radio_mix`, `radio_instrumental` (a doc tinha `radio_master`,
   `radio_instrumental_master`).
2. **MP3**: o *formulário* do site só tem 2 slots hoje (`extended_mp3`,
   `radio_mp3`), MAS o **backend** (`app/api/tracks/upload/route.js`) aceita
   QUALQUER campo `xf_*` (`FILE_LABELS[fkey] || fkey`, salvo no bucket
   `originals` + `metadata.files`). Então o app gera 1 MP3 por master/instrumental:
   - Extended Mix → `xf_extended_mp3`
   - Extended Instrumental → `xf_extended_instrumental_mp3` *(campo novo; o produtor
     vai adicionar esse input ao form do site depois, com `data-fkey="extended_instrumental_mp3"`)*
   - Radio Mix → `xf_radio_mp3` *(se houver)*
   - Radio Instrumental → `xf_radio_instrumental_mp3` *(se houver)*
3. `stems`/`midi`/`project` são `.zip` no upload.

## Endpoint

`POST /api/tracks/upload`, header `Authorization: Bearer <session.access_token>`,
body = `FormData`. Resposta: JSON; `res.ok` false → `data.error`; pode vir
`data.warnings[]` (arquivos > ~50MB não armazenados).
