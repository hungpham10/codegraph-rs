<script lang="ts">
	import { workspace } from '#lib/workspace.svelte';
	import { settings } from '#lib/settings';
	import { get } from 'svelte/store';

	let showSettings = $state(false);
	let s = $state({ ...get(settings) });

	function saveSettings() {
		settings.set({ endpoint: s.endpoint.trim(), apiKey: s.apiKey.trim() });
		showSettings = false;
		void workspace.refresh();
	}
</script>

<header class="flex items-center justify-between border-b border-neutral-800 bg-neutral-900/60 px-4 py-2">
	<div class="flex items-center gap-3">
		<span class="font-mono text-sm font-semibold text-neutral-100">codegraph</span>
		<span class="hidden text-xs text-neutral-500 sm:inline">research cockpit</span>
	</div>

	<div class="flex items-center gap-2">
		{#if workspace.status}
			<span class="hidden items-center gap-2 text-xs text-neutral-500 md:flex">
				<span>{workspace.status.symbols} symbols</span>
				<span class="text-neutral-700">·</span>
				<span>{workspace.status.files} files</span>
			</span>
		{/if}

		<span
			class="inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs {workspace.connected
				? 'border-emerald-800 bg-emerald-950/40 text-emerald-300'
				: 'border-red-900 bg-red-950/40 text-red-300'}"
		>
			<span
				class="size-1.5 rounded-full {workspace.connected ? 'bg-emerald-400' : 'bg-red-400'}"
			></span>
			{workspace.connected ? 'connected' : 'offline'}
		</span>

		<button
			class="rounded px-2 py-1 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
			onclick={() => workspace.refresh()}
			disabled={workspace.loading}
		>
			{workspace.loading ? '…' : 'refresh'}
		</button>
		<button
			class="rounded px-2 py-1 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
			onclick={() => (showSettings = !showSettings)}
		>
			settings
		</button>
	</div>
</header>

{#if showSettings}
	<div class="border-b border-neutral-800 bg-neutral-900 p-4">
		<div class="mx-auto flex max-w-2xl flex-col gap-3">
			<label class="flex flex-col gap-1 text-xs text-neutral-400">
				GraphQL endpoint (để trống = same-origin <code>/graphql</code>)
				<input
					class="rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-sm text-neutral-200"
					bind:value={s.endpoint}
					placeholder="http://127.0.0.1:8123/graphql"
				/>
			</label>
			<label class="flex flex-col gap-1 text-xs text-neutral-400">
				API key (nếu server bật <code>--api-key</code>)
				<input
					class="rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-sm text-neutral-200"
					bind:value={s.apiKey}
					placeholder="optional"
				/>
			</label>
			<div class="flex justify-end gap-2">
				<button
					class="rounded px-3 py-1 text-sm text-neutral-400 hover:bg-neutral-800"
					onclick={() => (showSettings = false)}>Cancel</button
				>
				<button
					class="rounded bg-sky-700 px-3 py-1 text-sm text-white hover:bg-sky-600"
					onclick={saveSettings}>Save</button
				>
			</div>
		</div>
	</div>
{/if}
