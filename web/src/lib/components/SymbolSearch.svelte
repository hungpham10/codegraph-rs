<script lang="ts">
	import { QS } from '#lib/graphql/queries';
	import type {
	Symbol,
	SearchSymbolResult,
	AnnotationSearchResult
} from '#lib/graphql/queries';
	import { gql, gqlError } from '#lib/graphql/client';
	import { kindColor, shortPath } from '#lib/format';

	let {
		onSelect,
		selectedId = null
	}: { onSelect: (s: Symbol) => void; selectedId?: string | null } = $props();

	let query = $state('');
	let kind = $state('');
	let mode = $state('CONTAINS');
	let source = $state<'name' | 'annotation'>('name');
	let results = $state<Symbol[]>([]);
	let total = $state(0);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let searched = $state(false);
	let timedOut = $state(false);
	let resume = $state<string | null>(null);
	let offset = $state(0);
	const PAGE = 50;

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

	async function run(nextOffset = 0, resumeId?: string) {
		if (!query.trim()) return;
		loading = true;
		error = null;
		searched = true;
		if (nextOffset === 0) {
			results = [];
			offset = 0;
		}
		try {
			if (source === 'annotation') {
				const data = await gql<{ graphcodeSearchByAnnotation: AnnotationSearchResult }>(
					QS.searchByAnnotation,
					{
						annotation: query.trim(),
						kind: kind || null,
						limit: PAGE,
						offset: nextOffset
					}
				);
				total = data.graphcodeSearchByAnnotation.total;
				results =
					nextOffset === 0
						? data.graphcodeSearchByAnnotation.symbols
						: [...results, ...data.graphcodeSearchByAnnotation.symbols];
				offset = nextOffset + data.graphcodeSearchByAnnotation.symbols.length;
			} else {
				const input: Record<string, unknown> = {
					query: query.trim(),
					mode,
					limit: PAGE,
					offset: nextOffset
				};
				if (kind) input.kind = kind;
				if (resumeId) input.resume = resumeId;
				const data = await gql<{ searchSymbol: SearchSymbolResult }>(QS.searchSymbol, { input });
				total = data.searchSymbol.total;
				timedOut = data.searchSymbol.timedOut;
				resume = data.searchSymbol.resume;
				results =
					nextOffset === 0 ? data.searchSymbol.symbols : [...results, ...data.searchSymbol.symbols];
				offset = nextOffset + data.searchSymbol.symbols.length;
			}
		} catch (e) {
			error = gqlError(e);
			if (nextOffset === 0) results = [];
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
				class="rounded border border-neutral-700 bg-neutral-950 px-2 py-1 text-xs text-neutral-300"
				bind:value={source}
			>
				<option value="name">by name</option>
				<option value="annotation">by annotation</option>
			</select>
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
				onclick={() => void run()}
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
				<div class="flex items-center justify-between px-3 pb-3 text-[11px] text-neutral-500">
					<span>Showing {results.length} of {total}</span>
					<button
						class="rounded border border-neutral-700 px-2 py-0.5 hover:bg-neutral-800"
						disabled={loading}
						onclick={() => void run(offset)}>Load more</button
					>
				</div>
			{/if}
			{#if timedOut && resume}
				<div class="px-3 pb-3">
					<button
						class="w-full rounded border border-amber-800 bg-amber-950/40 px-2 py-1 text-[11px] text-amber-300 hover:bg-amber-900/40"
						disabled={loading}
						onclick={() => void run(offset, resume ?? undefined)}>Search timed out — continue</button
					>
				</div>
			{/if}
		{/if}
	</div>
</div>
