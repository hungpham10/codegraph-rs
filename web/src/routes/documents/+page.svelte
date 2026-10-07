<script lang="ts">
	import { onMount } from 'svelte';
	import { QS, MS } from '#lib/graphql/queries';
	import type { DocInfo, DocNode, DocStats, DocPattern } from '#lib/graphql/queries';
	import { gql, gqlError } from '#lib/graphql/client';
	import { shortPath } from '#lib/format';
	import { workspace } from '#lib/workspace.svelte';

	let stats = $state<DocStats | null>(null);
	let docs = $state<DocInfo[]>([]);
	let patterns = $state<DocPattern[]>([]);

	let searchMode = $state<'path' | 'value'>('path');
	let query = $state('');
	let results = $state<DocNode[]>([]);
	let hydrated = $state<DocNode | null>(null);

	let ingestPath = $state('');
	let ingestFormat = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);
	let notice = $state<string | null>(null);

	onMount(refresh);

	async function refresh() {
		error = null;
		try {
			const [s, l, p] = await Promise.all([
				gql<{ graphdocStats: DocStats }>(QS.docStats),
				gql<{ graphdocList: DocInfo[] }>(QS.docList),
				gql<{ graphdocListPatterns: DocPattern[] }>(QS.docPatterns)
			]);
			stats = s.graphdocStats;
			docs = l.graphdocList;
			patterns = p.graphdocListPatterns;
		} catch (e) {
			error = gqlError(e);
		}
	}

	async function search() {
		if (!query.trim()) return;
		error = null;
		try {
			if (searchMode === 'path') {
				const d = await gql<{ graphdocSearch: DocNode[] }>(QS.docSearch, {
					pattern: query.trim(),
					depth: 1
				});
				results = d.graphdocSearch;
			} else {
				const d = await gql<{ graphdocSearchValue: DocNode[] }>(QS.docSearchValue, {
					query: query.trim(),
					limit: 50
				});
				results = d.graphdocSearchValue;
			}
		} catch (e) {
			error = gqlError(e);
			results = [];
		}
	}

	async function hydrate(id: string) {
		try {
			const d = await gql<{ graphdocHydrate: DocNode | null }>(QS.docHydrate, { id, depth: 4 });
			hydrated = d.graphdocHydrate;
		} catch (e) {
			error = gqlError(e);
		}
	}

	async function doIngest(dir: boolean) {
		if (!ingestPath.trim()) return;
		busy = true;
		notice = null;
		error = null;
		try {
			if (dir) {
				const d = await gql<{ graphdocIngestDir: { requested: number; ingested: number; failed: number } }>(MS.docIngestDir, {
					path: ingestPath.trim(),
					limit: 500
				});
				notice = `ingested ${d.graphdocIngestDir.ingested}/${d.graphdocIngestDir.requested} (failed ${d.graphdocIngestDir.failed})`;
			} else {
				const d = await gql<{ graphdocIngest: string }>(MS.docIngest, {
					path: ingestPath.trim(),
					format: ingestFormat.trim() || null
				});
				notice = d.graphdocIngest;
			}
			await refresh();
		} catch (e) {
			error = gqlError(e);
		} finally {
			busy = false;
		}
	}

	async function removeDoc(id: string) {
		try {
			await gql(MS.docRemove, { id });
			if (hydrated?.doc === id) hydrated = null;
			await refresh();
		} catch (e) {
			error = gqlError(e);
		}
	}

	async function mine() {
		busy = true;
		try {
			await gql(MS.docMinePatterns, { topK: 30, minCount: 3, maxDepth: 4 });
			await refresh();
		} catch (e) {
			error = gqlError(e);
		} finally {
			busy = false;
		}
	}
</script>

{#if !workspace.hasModule('documents')}
	<div class="flex h-full items-center justify-center p-6 text-sm text-neutral-500">
		Documents module is disabled. Enable it with <code>--ui-modules documents</code>.
	</div>
{:else}
	<div class="flex h-full flex-col">
	<!-- Stats + ingest -->
	<div class="border-b border-neutral-800 p-3">
		<div class="flex items-center gap-4">
			<h2 class="text-sm font-semibold text-neutral-200">Documents</h2>
			{#if stats}
				<span class="text-xs text-neutral-500">
					{stats.docs} docs · {stats.nodes} nodes
				</span>
			{/if}
			<button
				class="ml-auto rounded px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800"
				onclick={refresh}>reload</button
			>
			<button
				class="rounded px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800"
				onclick={mine}
				disabled={busy}>mine patterns</button
			>
		</div>
		<div class="mt-2 flex items-center gap-2">
			<input
				class="flex-1 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-xs text-neutral-200"
				placeholder="path to document file or directory…"
				bind:value={ingestPath}
			/>
			<input
				class="w-20 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-xs text-neutral-200"
				placeholder="format"
				bind:value={ingestFormat}
			/>
			<button
				class="rounded bg-sky-700 px-3 py-1 text-xs text-white hover:bg-sky-600 disabled:opacity-50"
				onclick={() => doIngest(false)}
				disabled={busy}>ingest</button
			>
			<button
				class="rounded border border-neutral-700 px-3 py-1 text-xs text-neutral-300 hover:bg-neutral-800 disabled:opacity-50"
				onclick={() => doIngest(true)}
				disabled={busy}>ingest dir</button
			>
		</div>
		{#if notice}
			<div class="mt-2 rounded border border-emerald-900 bg-emerald-950/40 p-2 text-xs text-emerald-300">
				{notice}
			</div>
		{/if}
		{#if error}
			<div class="mt-2 rounded border border-red-900 bg-red-950/40 p-2 text-xs text-red-300">
				{error}
			</div>
		{/if}
	</div>

	<div class="grid min-h-0 flex-1 grid-cols-[20rem_1fr] divide-x divide-neutral-800">
		<!-- Docs list + patterns -->
		<div class="thin-scroll min-h-0 overflow-auto p-2">
			<h3 class="px-1 py-1 text-[11px] font-semibold tracking-wide text-neutral-500 uppercase">
				Documents
			</h3>
			{#each docs as d (d.docId)}
				<div class="group flex items-center gap-2 rounded px-2 py-1 hover:bg-neutral-800/60">
					<button class="min-w-0 flex-1 text-left" onclick={() => hydrate(d.rootNodeId)}>
						<div class="truncate text-xs text-neutral-200" title={d.path}
							>{shortPath(d.path, 40)}</div
						>
						<div class="text-[10px] text-neutral-500">{d.format} · {d.nodes} nodes</div>
					</button>
					<button
						class="hidden shrink-0 text-[10px] text-red-400 group-hover:block"
						onclick={() => removeDoc(d.docId)}>rm</button
					>
				</div>
			{/each}
			{#if docs.length === 0}
				<div class="px-2 py-3 text-xs text-neutral-600">Chưa có document. Ingest một file.</div>
			{/if}

			{#if patterns.length}
				<h3 class="mt-3 px-1 py-1 text-[11px] font-semibold tracking-wide text-neutral-500 uppercase">
					Patterns
				</h3>
				{#each patterns as p (p.patternId)}
					<div class="rounded px-2 py-1 text-[11px]">
						<span class="font-mono text-neutral-300">{p.tokens.join(' › ')}</span>
						<span class="ml-1 text-neutral-600"
							>P{p.patternId} · {p.docCount} docs · freq {p.docFreq.toFixed(2)}</span
						>
					</div>
				{/each}
			{/if}
		</div>

		<!-- Search + hydrate -->
		<div class="flex min-h-0 flex-col">
			<div class="flex items-center gap-2 border-b border-neutral-800 p-2">
				<select
					class="rounded border border-neutral-700 bg-neutral-950 px-2 py-1 text-xs text-neutral-300"
					bind:value={searchMode}
				>
					<option value="path">path</option>
					<option value="value">value</option>
				</select>
				<input
					class="flex-1 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-xs text-neutral-200"
					placeholder={searchMode === 'path' ? 'spec.replicas' : 'search scalar value…'}
					bind:value={query}
					onkeydown={(e) => e.key === 'Enter' && search()}
				/>
				<button
					class="rounded bg-sky-700 px-3 py-1 text-xs text-white hover:bg-sky-600">Search</button
				>
			</div>

			<div class="thin-scroll min-h-0 flex-1 overflow-auto p-3">
				{#if hydrated}
					<div class="mb-3 flex items-center justify-between">
						<h3 class="font-mono text-xs text-neutral-300">doc {hydrated.doc}</h3>
						<button
							class="text-[11px] text-neutral-500 hover:text-neutral-300"
							onclick={() => (hydrated = null)}>close</button
						>
					</div>
					<div class="font-mono text-xs leading-relaxed">
						{@render node(hydrated, 0)}
					</div>
				{:else if results.length}
					{#each results as r (r.id)}
						<button
							class="flex w-full items-center gap-2 rounded px-2 py-1 text-left hover:bg-neutral-800/60"
							onclick={() => hydrate(r.id)}
						>
							<span class="font-mono text-xs text-sky-300">{r.path.join('.') || r.key || r.id}</span>
							{#if r.kind}<span class="rounded bg-neutral-800 px-1 text-[10px] text-neutral-400"
									>{r.kind}</span
								>{/if}
							{#if r.value}<span class="truncate text-xs text-neutral-400">{r.value}</span>{/if}
						</button>
					{/each}
				{:else}
					<div class="text-sm text-neutral-500">
						Chọn document bên trái hoặc search để xem cây giá trị.
					</div>
				{/if}
			</div>
		</div>
	</div>
</div>
{/if}

{#snippet node(n: DocNode, depth: number)}
	{@const pad = depth * 14}
	<div style="padding-left:{pad}px" class="py-0.5">
		<span class="text-neutral-500">{n.key ?? (n.path.length ? n.path[n.path.length - 1] : '•')}</span>
		{#if n.value}
			<span class="text-neutral-600">=</span>
			<span class="text-emerald-300">{n.value}</span>
		{:else}
			<span class="text-neutral-700">({n.kind})</span>
		{/if}
		{#each n.children as c (c.id)}
			{@render node(c, depth + 1)}
		{/each}
	</div>
{/snippet}
