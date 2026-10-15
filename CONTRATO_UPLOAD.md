# Contrato do upload — app ↔ site

O que o app chama (`src-tauri/src/uploader.rs`, `commands.rs`). Todas as
chamadas levam `Authorization: Bearer <access_token>` e respondem JSON com
`success` / `error`. A lista de slots aceites, as extensões por slot e os tetos
de tamanho vivem no site, em `lib/upload-exts.js` (`FILE_LABELS`,
`allowedExts`, `maxBytesFor`) — não copiar a tabela para aqui, diverge.

1. **`POST /api/app/draft-create`** — `multipart/form-data` com os campos de
   texto (`genre`, `bpm`, `music_key`, `daw`, `has_project_file`, `metadata`
   em JSON) e a capa (`cover`, até 8MB). O preço não vai: num rascunho o site
   pede-o ao produtor, track a track. Responde `{ success, id,
   warnings[] }`; o rascunho fica com `status = draft`.
2. Para cada ficheiro (master e extras), um de cada vez:
   - **`POST /api/app/draft-file-url`** `{ draft_id, fkey, filename }` — só
     valida o rascunho (dono/estado) e o slot antes de enviar bytes.
   - **`POST /api/tracks/upload-multipart`** — envio direto ao R2 em partes de
     64MB: `create { fkey, filename }` → `{ uploadId, path }`;
     `sign { path, uploadId, parts[] }` → `{ urls: [{ part, url }] }`;
     `PUT` de cada parte na URL assinada; `complete { path, uploadId }` →
     `{ parts }` (ou `{ already: true }` num complete repetido);
     `abort { path, uploadId }` em qualquer falha.
   - **`POST /api/app/draft-file-commit`** `{ draft_id, fkey, path, name }` —
     regista o ficheiro no rascunho.
3. O app abre `upload.html?edit=<id>` no browser; o produtor termina lá.

`fkey`: o master usa `master`; os extras usam o campo sem o prefixo `xf_`
(`xf_stems` → `stems`). MIDI, stems e projeto só em `.zip`.

Perfil: **`GET /api/user/profile`** → `{ profile }` (nome e avatar no topo).
