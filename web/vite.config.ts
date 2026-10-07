import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';

// Ứng dụng là SPA client-only: adapter-static với fallback `200.html`.
// Khi serve trong binary Rust (crate `codegraph-web`) fallback này được phục
// vụ cho mọi route không khớp asset. `assets` ghi thẳng vào crate để
// rust-embed nhúng lúc `cargo build`.
// Backend GraphQL khi `codegraph serve --graphql` bind mặc định 0.0.0.0:8123.
// Dev proxy giữ UI same-origin (endpoint = location.origin) như lúc nhúng
// vào binary — tránh CORS và mô phỏng đúng môi trường production.
const backend = process.env.CODEGRAPH_BACKEND ?? 'http://127.0.0.1:8123';

export default defineConfig({
	resolve: {
		alias: {
			'#lib': fileURLToPath(new URL('./src/lib', import.meta.url))
		}
	},
	server: {
		proxy: {
			'/graphql': { target: backend, changeOrigin: true },
			'/health': { target: backend, changeOrigin: true }
		}
	},
	plugins: [
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},

			// SPA: build ra static, không cần Node server lúc chạy.
			adapter: adapter({
				pages: '../crates/codegraph-web/assets',
				assets: '../crates/codegraph-web/assets',
				fallback: '200.html',
				precompress: false,
				strict: false
			})
		})
	]
});
