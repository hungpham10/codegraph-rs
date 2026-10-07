<script lang="ts">
	import { QS } from '#lib/graphql/queries';
	import type { Symbol, SearchSymbolResult } from '#lib/graphql/queries';
	import { gql, gqlError } from '#lib/graphql/client';
	import { kindColor, shortPath } from '#lib/format';

	let {
		onSelect,
		selectedId = null
	}: { onSelect: (s: Symbol) => void; selectedId?: string | null } = $props();

	let query = $state('');
	let kind = $state('');
	let mode = $state('CONTAINS');
	let results = $state<Symbol[]>([]);
	let total = $state(0);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let searched = $state(false);

	const KINDS = [
		'',
		'FUNCTION',
		'METHOD',
		'CLASS',
		'INTERFACE',
		'ENUM',
		'MODULE',
		'VARIABLE',
		'FIELD'
	];
	const MODES = ['CONTAINS', 'PREFIX', 'SUFFIX', 'EXACT', 'SEMANTIC', 'HYBRID'];

	async function run() {
		if (!query.trim()) return;
		loading = true;
		error = null;
		searched = true;
		try {
			const input: Record<string, unknown> = { query: query.trim(), mode, limit: 50 };
			if (kind) input.kind = kind;
			const data = await gql<{ searchSymbol: SearchSymbolResult }>(QS.searchSymbol, { input });
			results = data.searchSymbol.symbols;
			total = data.searchSymbol.total;
		} catch (e) {
			error = gqlError(e);
			results = [];
		} finally {
			loading = false;
		}
	}
</script>

<div class="flex h-full flex-col">
	<div class="flex flex-col gap-2 border-b border-neutral-800 p-3">
		<input
			class="w-full rounded border border-neutral-700 bg-neutral-950 px-3 py-1.5 font-mono text-sm text-neutral-100 placeholder-neutral-600 focus:border-sky-700 focus:outline-none"
			placeholder="search symbol…"
			bind:value={query}
			onkeydown={(e) => e.key === 'Enter' && run()}
		/>
		<div class="flex gap-2">
			<select
				class="flex-1 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 text-xs text-neutral-300"
				bind:value={kind}
			>
				{#each KINDS as k (k)}
					<option value={k}>{k || 'any kind'}</option>
				{/each}
			</select>
			<select
				class="flex-1 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 text-xs text-neutral-300"
				bind:value={mode}
			>
				{#each MODES as m (m)}
					<option value={m}>{m.toLowerCase()}</option>
				{/each}
			</select>
			<button
				class="rounded bg-sky-700 px-3 py-1 text-xs font-medium text-white hover:bg-sky-600 disabled:opacity-50"
				onclick={run}
				disabled={loading}
			>
				{loading ? '…' : 'Search'}
			</button>
		</div>
	</div>

	<div class="thin-scroll min-h-0 flex-1 overflow-auto">
		{#if error}
			<div class="m-3 rounded border border-red-900 bg-red-950/40 p-2 text-xs text-red-300"
				>{error}</div
			>
		{:else if searched && results.length === 0 && !loading}
			<div class="p-3 text-xs text-neutral-500">No symbols matched.</div>
		{:else}
			<div class="p-1.5">
				{#each results as s (s.id)}
					<button
						class="flex w-full flex-col gap-0.5 rounded px-2 py-1.5 text-left transition-colors {s.id ===
						selectedId
							? 'bg-sky-950/60 ring-1 ring-sky-800'
							: 'hover:bg-neutral-800/60'}"
						onclick={() => onSelect(s)}
					>
						<div class="flex items-center gap-2">
							<span
								class="rounded border px-1 font-mono text-[10px] {kindColor(s.kind)}">{s.kind}</span
							>
							<span class="truncate font-mono text-sm text-neutral-100">{s.name}</span>
						</div>
						<span class="truncate text-[11px] text-neutral-500"
							>{shortPath(s.file, 60)}:{s.line}</span
						>
					</button>
				{/each}
			</div>
			{#if total > results.length}
				<div class="px-3 pb-3 text-[11px] text-neutral-500">
					Showing {results.length} of {total}. Narrow the query for more.
				</div>
			{/if}
		{/if}
	</div>
</div>
