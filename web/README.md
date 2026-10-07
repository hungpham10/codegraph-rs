# codegraph web UI

Giao diện **research / review code** cho codegraph — SvelteKit SPA (client-only),
Tailwind v4, mermaid. Backend là GraphQL của `codegraph serve --graphql`.

## Modules

| Route | Module | Mô tả |
|---|---|---|
| `/` | **Explore** | search symbol, xem flow (mermaid), callers/callees/impact, context markdown, files/status |
| `/review` | **Review** | dán unified diff → phân tích symbol/flow bị chạm + trace delta |
| `/documents` | **Documents** | duyệt/search/hydrate structured docs (YAML/JSON/TOML/HCL), ingest, mine patterns |

## Develop

```sh
npm install
npm run dev        # http://localhost:5173 — proxy /graphql → 127.0.0.1:8123
```

Backend (1 terminal khác):

```sh
codegraph serve --graphql --mermaid --addr 127.0.0.1:8123 --path <repo>
```

Dev proxy đọc biến `CODEGRAPH_BACKEND` (mặc định `http://127.0.0.1:8123`).

## Build & nhúng vào binary

```sh
scripts/build-web.sh
```

Script chạy `npm ci` + `svelte-check` + `vite build`, ghi asset vào
`crates/codegraph-web/assets/` (gitignored). `rust-embed` nhúng folder đó lúc
`cargo build` → UI đi kèm binary, phục vụ tại `/` (same-origin, không CORS).

Tắt UI khi chỉ cần API: `codegraph serve --graphql --no-web`.

## Check

```sh
npm run check   # svelte-check
```
