import { writable } from 'svelte/store';

const STORAGE_KEY = 'codegraph.settings';

export interface Settings {
	/** Override endpoint GraphQL (rỗng = same-origin `/graphql`). */
	endpoint: string;
	/** API key (`Authorization: Bearer …`) nếu server bật `--api-key`. */
	apiKey: string;
}

function load(): Settings {
	if (typeof localStorage === 'undefined') return { endpoint: '', apiKey: '' };
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (!raw) return { endpoint: '', apiKey: '' };
		const parsed = JSON.parse(raw) as Partial<Settings>;
		return { endpoint: parsed.endpoint ?? '', apiKey: parsed.apiKey ?? '' };
	} catch {
		return { endpoint: '', apiKey: '' };
	}
}

/** Settings bền vững (localStorage) — endpoint + api key. */
export const settings = writable<Settings>(load());

if (typeof localStorage !== 'undefined') {
	settings.subscribe((value) => {
		localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
	});
}
