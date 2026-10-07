/** Helpers hiển thị dùng chung. */

const KIND_COLORS: Record<string, string> = {
	FUNCTION: 'bg-sky-900/50 text-sky-300 border-sky-800',
	METHOD: 'bg-sky-900/40 text-sky-300 border-sky-800',
	CLASS: 'bg-violet-900/50 text-violet-300 border-violet-800',
	INTERFACE: 'bg-violet-900/40 text-violet-300 border-violet-800',
	ENUM: 'bg-fuchsia-900/40 text-fuchsia-300 border-fuchsia-800',
	MODULE: 'bg-amber-900/40 text-amber-300 border-amber-800',
	VARIABLE: 'bg-neutral-800 text-neutral-300 border-neutral-700',
	CONSTANT: 'bg-neutral-800 text-neutral-300 border-neutral-700',
	FIELD: 'bg-emerald-900/40 text-emerald-300 border-emerald-800',
	PARAMETER: 'bg-emerald-900/30 text-emerald-300 border-emerald-800',
	FILE: 'bg-neutral-800 text-neutral-400 border-neutral-700',
	CONFIG: 'bg-neutral-800 text-neutral-400 border-neutral-700'
};

export function kindColor(kind: string): string {
	return KIND_COLORS[kind] ?? 'bg-neutral-800 text-neutral-300 border-neutral-700';
}

export function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function shortPath(p: string, max = 48): string {
	if (p.length <= max) return p;
	return '…' + p.slice(p.length - max + 1);
}

/** Effect → nhãn ngắn gọn. */
export const EFFECT_LABELS: Record<string, string> = {
	NONE: '',
	SQL_QUERY: 'sql',
	SQL_WRITE: 'sql-write',
	CACHE_READ: 'cache',
	CACHE_WRITE: 'cache-write',
	HTTP_CALL: 'http',
	EVENT_EMIT: 'emit',
	FILE_READ: 'read',
	FILE_WRITE: 'write',
	LOG: 'log'
};
