<script lang="ts">
	import { shortPath } from '#lib/format';

	type Row = { id: string; name: string; file?: string; line: number };

	let {
		title = '',
		items = [],
		onSelect
	}: { title?: string; items: Row[]; onSelect: (s: Row) => void } = $props();
</script>

{#if title}
	<h3 class="mb-1 text-xs font-semibold tracking-wide text-neutral-400 uppercase">
		{title}
		<span class="text-neutral-600">({items.length})</span>
	</h3>
{/if}
{#if items.length === 0}
	<div class="mb-3 px-2 text-[11px] text-neutral-600">none</div>
{:else}
	<div class="mb-3 flex flex-col">
		{#each items as s (s.id)}
			<button
				class="flex flex-col gap-0.5 rounded px-2 py-1 text-left hover:bg-neutral-800/60"
				onclick={() => onSelect(s)}
			>
				<span class="truncate font-mono text-xs text-neutral-200">{s.name}</span>
				{#if s.file}
					<span class="truncate text-[10px] text-neutral-500">{shortPath(s.file, 40)}:{s.line}</span>
				{:else}
					<span class="truncate text-[10px] text-neutral-500">L{s.line}</span>
				{/if}
			</button>
		{/each}
	</div>
{/if}
