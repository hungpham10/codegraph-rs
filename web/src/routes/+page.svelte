<script lang="ts">
	import { onMount } from 'svelte';
	import { workspace } from '#lib/workspace.svelte';
	import type { Symbol } from '#lib/graphql/queries';
	import SymbolSearch from '#lib/components/SymbolSearch.svelte';
	import SymbolList from '#lib/components/SymbolList.svelte';
	import SymbolDetail from '#lib/components/SymbolDetail.svelte';
	import FileBrowser from '#lib/components/FileBrowser.svelte';
	import ConnectPanel from '#lib/components/ConnectPanel.svelte';

	let selected = $state<Symbol | null>(null);
	let showFiles = $state(false);
	let leftCollapsed = $state(false);
	let tab = $state<'browse' | 'search'>('browse');

	onMount(() => {
		void workspace.refresh();
		void workspace.loadUiConfig();
	});
</script>

{#if !workspace.hasModule('explore')}
	<div class="flex h-full items-center justify-center p-6 text-sm text-neutral-500">
		Explore module is disabled. Enable it with <code>--ui-modules explore</code>.
	</div>
{:else if !workspace.connected && !workspace.loading}
	<ConnectPanel />
{:else}
	<div class="flex h-full">
		<!-- Cột trái: browse / search (thu gọn được) -->
		{#if !leftCollapsed}
			<div class="flex w-80 shrink-0 flex-col border-r border-neutral-800">
				<div class="flex items-center gap-1 border-b border-neutral-800 px-2 py-1.5">
					<button
						class="rounded px-3 py-1 text-xs font-medium {tab === 'browse'
							? 'bg-neutral-800 text-neutral-100'
							: 'text-neutral-400 hover:text-neutral-200'}"
						onclick={() => (tab = 'browse')}>Browse</button
					>
					<button
						class="rounded px-3 py-1 text-xs font-medium {tab === 'search'
							? 'bg-neutral-800 text-neutral-100'
							: 'text-neutral-400 hover:text-neutral-200'}"
						onclick={() => (tab = 'search')}>Search</button
					>
					<button
						class="ml-auto rounded px-1.5 py-0.5 text-xs text-neutral-500 hover:bg-neutral-800 hover:text-neutral-300"
						title="Collapse left panel"
						onclick={() => (leftCollapsed = true)}>‹</button
					>
				</div>
				<div class="min-h-0 flex-1">
					{#if tab === 'browse'}
						<SymbolList onSelect={(s) => (selected = s)} />
					{:else}
						<SymbolSearch onSelect={(s) => (selected = s)} selectedId={selected?.id ?? null} />
					{/if}
				</div>
			</div>
		{:else}
			<button
				class="w-6 shrink-0 border-r border-neutral-800 text-neutral-600 hover:bg-neutral-800/60 hover:text-neutral-300"
				title="Expand left panel"
				onclick={() => (leftCollapsed = false)}>›</button
			>
		{/if}

		<!-- Chi tiết (graph rộng nhất) -->
		<div class="min-w-0 flex-1">
			{#if selected}
				<SymbolDetail symbol={selected} onSelect={(s) => (selected = s)} />
			{:else}
				<div class="flex h-full flex-col items-center justify-center gap-2 text-neutral-500">
					<p class="text-sm">Chọn một symbol để xem flow, call graph và context.</p>
					<button
						class="rounded border border-neutral-700 px-3 py-1 text-xs hover:bg-neutral-800"
						onclick={() => (showFiles = !showFiles)}
					>
						{showFiles ? 'Hide files' : 'Browse files'}
					</button>
				</div>
			{/if}
		</div>

		<!-- Files (thu gọn được) -->
		{#if showFiles}
			<div class="flex w-80 shrink-0 flex-col border-l border-neutral-800">
				<div class="flex items-center justify-end border-b border-neutral-800 px-2 py-1">
					<button
						class="rounded px-1.5 py-0.5 text-xs text-neutral-500 hover:bg-neutral-800 hover:text-neutral-300"
						title="Collapse files panel"
						onclick={() => (showFiles = false)}>›</button
					>
				</div>
				<div class="min-h-0 flex-1">
					<FileBrowser />
				</div>
			</div>
		{/if}
	</div>
{/if}
