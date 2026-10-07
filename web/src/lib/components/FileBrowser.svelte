<script lang="ts">
	import { onMount } from 'svelte';
	import { QS } from '#lib/graphql/queries';
	import type { FileInfo, Status } from '#lib/graphql/queries';
	import { gql } from '#lib/graphql/client';
	import { formatBytes, shortPath } from '#lib/format';

	let files = $state<FileInfo[]>([]);
	let status = $state<Status | null>(null);
	let filter = $state('');
	let loading = $state(false);

	onMount(load);

	async function load() {
		loading = true;
		try {
			const [f, s] = await Promise.all([
				gql<{ graphcodeFiles: FileInfo[] }>(QS.files, { prefix: '' }),
				gql<{ status: Status }>(QS.status)
			]);
			files = f.graphcodeFiles;
			status = s.status;
		} finally {
			loading = false;
		}
	}

	let filtered = $derived(
		filter
			? files.filter((f) => f.path.toLowerCase().includes(filter.toLowerCase()))
			: files
	);
</script>

<div class="flex h-full flex-col">
	<div class="flex items-center justify-between border-b border-neutral-800 px-3 py-2">
		<h2 class="text-sm font-semibold text-neutral-200">Files</h2>
		<button
			class="rounded px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800"
			onclick={load}>reload</button
		>
	</div>

	{#if status}
		<div class="grid grid-cols-2 gap-x-4 gap-y-0.5 border-b border-neutral-800 px-3 py-2 text-xs">
			<span class="text-neutral-500">symbols</span><span class="text-right font-mono text-neutral-200"
				>{status.symbols}</span
			>
			<span class="text-neutral-500">chains</span><span class="text-right font-mono text-neutral-200"
				>{status.chains}</span
			>
			<span class="text-neutral-500">edges</span><span class="text-right font-mono text-neutral-200"
				>{status.edges}</span
			>
			<span class="text-neutral-500">files</span><span class="text-right font-mono text-neutral-200"
				>{status.files}</span
			>
		</div>
	{/if}

	<div class="border-b border-neutral-800 p-2">
		<input
			class="w-full rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-xs text-neutral-200"
			placeholder="filter path…"
			bind:value={filter}
		/>
	</div>

	<div class="thin-scroll min-h-0 flex-1 overflow-auto p-1.5">
		{#if loading && files.length === 0}
			<div class="p-2 text-xs text-neutral-500">Loading…</div>
		{:else}
			{#each filtered as f (f.path)}
				<div class="flex items-center justify-between rounded px-2 py-1 hover:bg-neutral-800/60">
					<span class="truncate font-mono text-xs text-neutral-300" title={f.path}
						>{shortPath(f.path, 44)}</span
					>
					<span class="ml-2 shrink-0 text-[10px] text-neutral-600"
						>{f.lines}L · {formatBytes(f.bytes)}</span
					>
				</div>
			{/each}
			{#if filtered.length === 0}
				<div class="p-2 text-xs text-neutral-500">No files.</div>
			{/if}
		{/if}
	</div>
</div>
