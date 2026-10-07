<script lang="ts">
	import { QS } from '#lib/graphql/queries';
	import type { Symbol, ListResult } from '#lib/graphql/queries';
	import { gql, gqlError } from '#lib/graphql/client';
	import { kindColor, shortPath } from '#lib/format';

	let { onSelect }: { onSelect: (s: Symbol) => void } = $props();

	const KINDS = [
		'FUNCTION',
		'METHOD',
		'CLASS',
		'INTERFACE',
		'ENUM',
		'MODULE',
		'VARIABLE',
		'FIELD'
	];

	let kind = $state('FUNCTION');
	let limit = 50;
	let offset = $state(0);
	let items = $state<Symbol[]>([]);
	let total = $state(0);
	let loading = $state(false);
	let error = $state<string | null>(null);

	let hasMore = $derived(offset + items.length < total);

	async function load() {
		loading = true;
		error = null;
		try {
			const data = await gql<{ graphcodeListSymbols: ListResult }>(QS.listSymbols, {
				kind,
				limit,
				offset
			});
			items = data.graphcodeListSymbols.items;
			total = data.graphcodeListSymbols.total;
		} catch (e) {
			error = gqlError(e);
			items = [];
		} finally {
			loading = false;
		}
	}

	function pickKind(k: string) {
		kind = k;
		offset = 0;
		load();
	}

	function prev() {
		offset = Math.max(0, offset - limit);
		load();
	}

	function next() {
		if (!hasMore) return;
		offset += limit;
		load();
	}

	// Load lần đầu + khi kind đổi.
	$effect(() => {
		void kind;
		offset = 0;
		load();
	});
</script>

<div class="flex h-full flex-col">
	<div class="flex flex-col gap-2 border-b border-neutral-800 p-3">
		<select
			class="w-full rounded border border-neutral-700 bg-neutral-950 px-2 py-1 text-xs text-neutral-300"
			bind:value={kind}
			onchange={(e) => pickKind(e.currentTarget.value)}
		>
			{#each KINDS as k (k)}
				<option value={k}>{k.toLowerCase()}</option>
			{/each}
		</select>
		<div class="flex items-center justify-between text-[11px] text-neutral-500">
			<span>{total} {kind.toLowerCase()}s</span>
			<div class="flex gap-1">
				<button
					class="rounded px-2 py-0.5 hover:bg-neutral-800 disabled:opacity-40"
					onclick={prev}
					disabled={offset === 0 || loading}>‹ prev</button
				>
				<button
					class="rounded px-2 py-0.5 hover:bg-neutral-800 disabled:opacity-40"
					onclick={next}
					disabled={!hasMore || loading}>next ›</button
				>
			</div>
		</div>
	</div>

	<div class="thin-scroll min-h-0 flex-1 overflow-auto">
		{#if error}
			<div class="m-3 rounded border border-red-900 bg-red-950/40 p-2 text-xs text-red-300"
				>{error}</div
			>
		{:else if loading && items.length === 0}
			<div class="p-3 text-xs text-neutral-500">Loading…</div>
		{:else if items.length === 0}
			<div class="p-3 text-xs text-neutral-500">No {kind.toLowerCase()}s.</div>
		{:else}
			<div class="p-1.5">
				{#each items as s (s.id)}
					<button
						class="flex w-full flex-col gap-0.5 rounded px-2 py-1.5 text-left hover:bg-neutral-800/60"
						onclick={() => onSelect(s)}
					>
						<div class="flex items-center gap-2">
							<span class="rounded border px-1 font-mono text-[10px] {kindColor(s.kind)}"
								>{s.kind}</span
							>
							<span class="truncate font-mono text-sm text-neutral-100">{s.name}</span>
						</div>
						<span class="truncate text-[11px] text-neutral-500"
							>{shortPath(s.file, 60)}:{s.line}</span
						>
					</button>
				{/each}
			</div>
		{/if}
	</div>
</div>
