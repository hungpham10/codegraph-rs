<script lang="ts">
	import { onMount } from 'svelte';
	import { QS, MS } from '#lib/graphql/queries';
	import { gql, gqlError } from '#lib/graphql/client';
	import { workspace } from '#lib/workspace.svelte';
	import MermaidView from '#lib/components/MermaidView.svelte';
	import DiffView from '#lib/components/DiffView.svelte';

	type Mode = 'branches' | 'paste';
	type View = 'graph' | 'diff';

	interface Report {
		summary: {
			filesInDiff: number;
			filesMatched: number;
			symbolsAffected: number;
			flowsAffected: number;
			newFiles: string[];
			unmatchedFiles: string[];
		};
	}

	let mode = $state<Mode>('branches');
	let view = $state<View>('graph');

	// Branch compare
	let branches = $state<string[]>([]);
	let base = $state('');
	let head = $state('');
	let entry = $state('');
	let compareResult = $state<{
		diff: string;
		report: Report | null;
		diagram: string | null;
		baseNote?: string;
	} | null>(null);

	// Paste diff
	let pastedDiff = $state('');
	let pastedEntry = $state('');
	let pastedBase = $state('');
	let simMode = $state<'diff' | 'simulate' | 'origin'>('diff');
	let pasteResult = $state<string | null>(null);

	let error = $state<string | null>(null);
	let loading = $state(false);

	onMount(async () => {
		try {
			const d = await gql<{ gitBranches: string[] }>(QS.gitBranches);
			branches = d.gitBranches;
			if (branches.length) {
				base = branches[0];
				head = branches[1] ?? branches[0];
			}
		} catch {
			// không phải repo — bỏ qua
		}
	});

	async function compare() {
		if (!base) return;
		loading = true;
		error = null;
		compareResult = null;
		try {
			const args: Record<string, unknown> = { base };
			if (head && head !== base) args.head = head;
			if (entry.trim()) args.entry = entry.trim();
			const d = await gql<{ graphcodeBranchCompare: string }>(MS.branchCompare, { args });
			const parsed = JSON.parse(d.graphcodeBranchCompare);
			let diagram: string | null = null;
			let baseNote: string | undefined;
			if (typeof parsed.mermaid === 'string') diagram = parsed.mermaid;
			else if (parsed.mermaid?.diagram) {
				diagram = parsed.mermaid.diagram;
				baseNote = parsed.mermaid.base_index_note;
			}
			compareResult = { diff: parsed.diff ?? '', report: parsed.report ?? null, diagram, baseNote };
			view = diagram ? 'graph' : 'diff';
		} catch (e) {
			error = gqlError(e);
		} finally {
			loading = false;
		}
	}

	async function runPaste() {
		loading = true;
		error = null;
		pasteResult = null;
		try {
			const args: Record<string, unknown> = {};
			if (pastedDiff.trim()) args.diff = pastedDiff;
			if (pastedEntry.trim()) args.entry = pastedEntry.trim();
			if (pastedBase.trim()) {
				args.baseRef = pastedBase.trim();
				args.ref = pastedBase.trim();
			}
			let data: Record<string, string>;
			if (simMode === 'diff') {
				data = await gql(MS.diff, { args });
				pasteResult = data.graphcodeDiff;
			} else if (simMode === 'simulate') {
				data = await gql(MS.diffSimulate, { args });
				pasteResult = data.graphcodeDiffSimulate;
			} else {
				data = await gql(MS.originSimulate, { args });
				pasteResult = data.graphcodeOriginSimulate;
			}
		} catch (e) {
			error = gqlError(e);
		} finally {
			loading = false;
		}
	}

	let prettyPaste = $derived.by(() => {
		if (!pasteResult) return '';
		try {
			return JSON.stringify(JSON.parse(pasteResult), null, 2);
		} catch {
			return pasteResult;
		}
	});
</script>

{#if !workspace.hasModule('review')}
	<div class="flex h-full items-center justify-center p-6 text-sm text-neutral-500">
		Review module is disabled. Enable it with <code>--ui-modules review</code>.
	</div>
{:else}
	<div class="flex h-full flex-col">
		<!-- Toolbar -->
		<div class="flex items-center gap-1 border-b border-neutral-800 px-3 py-1.5">
			<button
				class="rounded px-3 py-1 text-xs font-medium {mode === 'branches'
					? 'bg-neutral-800 text-neutral-100'
					: 'text-neutral-400 hover:text-neutral-200'}"
				onclick={() => (mode = 'branches')}>Branches</button
			>
			<button
				class="rounded px-3 py-1 text-xs font-medium {mode === 'paste'
					? 'bg-neutral-800 text-neutral-100'
					: 'text-neutral-400 hover:text-neutral-200'}"
				onclick={() => (mode = 'paste')}>Paste diff</button
			>
		</div>

		{#if error}
			<div class="border-b border-red-900 bg-red-950/40 px-3 py-2 text-xs text-red-300">{error}</div>
		{/if}

		{#if mode === 'branches'}
			<!-- Branch compare controls -->
			<div class="flex flex-wrap items-center gap-2 border-b border-neutral-800 px-3 py-2 text-xs">
				<label class="flex items-center gap-1 text-neutral-500">
					base
					<select
						class="rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-neutral-200"
						bind:value={base}
					>
						{#if branches.length === 0}<option value="">(no git repo)</option>{/if}
						{#each branches as b (b)}<option value={b}>{b}</option>{/each}
					</select>
				</label>
				<span class="text-neutral-600">→</span>
				<label class="flex items-center gap-1 text-neutral-500">
					head
					<select
						class="rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-neutral-200"
						bind:value={head}
					>
						<option value="">working tree</option>
						{#each branches as b (b)}<option value={b}>{b}</option>{/each}
					</select>
				</label>
				<label class="flex items-center gap-1 text-neutral-500">
					entry <span class="text-neutral-600">(optional)</span>
					<input
						class="w-40 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-neutral-200"
						placeholder="fn name → 2-color graph"
						bind:value={entry}
					/>
				</label>
				<button
					class="ml-auto rounded bg-sky-700 px-3 py-1 font-medium text-white hover:bg-sky-600 disabled:opacity-50"
					onclick={compare}
					disabled={loading || !base}>{loading ? 'Comparing…' : 'Compare'}</button
				>
			</div>

			{#if compareResult}
				<!-- Summary -->
				{#if compareResult.report}
					{@const s = compareResult.report.summary}
					<div class="flex items-center gap-4 border-b border-neutral-800 px-3 py-1.5 text-xs">
						<span class="text-neutral-500"
							>files <span class="text-neutral-200">{s.filesMatched}/{s.filesInDiff}</span></span
						>
						<span class="text-neutral-500"
							>symbols <span class="text-amber-300">{s.symbolsAffected}</span></span
						>
						<span class="text-neutral-500"
							>flows <span class="text-sky-300">{s.flowsAffected}</span></span
						>
						{#if s.newFiles.length}
							<span class="text-emerald-400">+{s.newFiles.length} new files</span>
						{/if}
						<div class="ml-auto flex items-center gap-2">
							<div class="flex overflow-hidden rounded border border-neutral-700">
								<button
									class="px-2 py-0.5 {view === 'graph'
										? 'bg-neutral-700 text-neutral-100'
										: 'text-neutral-400 hover:bg-neutral-800'}"
									onclick={() => (view = 'graph')}
									disabled={!compareResult.diagram}>Graph</button
								>
								<button
									class="px-2 py-0.5 {view === 'diff'
										? 'bg-neutral-700 text-neutral-100'
										: 'text-neutral-400 hover:bg-neutral-800'}"
									onclick={() => (view = 'diff')}>Diff</button
								>
							</div>
						</div>
					</div>
					{#if compareResult.baseNote}
						<div class="border-b border-amber-900/50 bg-amber-950/20 px-3 py-1 text-[11px] text-amber-300">
							base index: {compareResult.baseNote}
						</div>
					{/if}
				{/if}

				<div class="min-h-0 flex-1">
					{#if view === 'graph' && compareResult.diagram}
						<div class="flex h-full flex-col">
							<div class="flex items-center gap-3 border-b border-neutral-800 px-3 py-1 text-[11px]">
								<span class="text-emerald-400">▢ added</span>
								<span class="text-red-400">▢ removed</span>
								<span class="text-neutral-500">mermaid · {base} → {head || 'working tree'}</span>
							</div>
							<div class="min-h-0 flex-1">
								<MermaidView code={compareResult.diagram} title="branch diff · {entry}" />
							</div>
						</div>
					{:else}
						<DiffView diff={compareResult.diff} />
					{/if}
				</div>
			{:else}
				<div class="flex h-full items-center justify-center text-sm text-neutral-500">
					Chọn 2 branch rồi Compare. Điền <code>entry</code> để xem graph 2 màu.
				</div>
			{/if}
		{:else}
			<!-- Paste diff mode -->
			<div class="grid min-h-0 flex-1 grid-cols-2 divide-x divide-neutral-800">
				<div class="flex min-h-0 flex-col">
					<div class="flex items-center gap-1 border-b border-neutral-800 px-3 py-1.5">
						{#each [['diff', 'Analyze'], ['simulate', 'Simulate'], ['origin', 'Ref vs worktree']] as [m, label] (m)}
							<button
								class="rounded px-2 py-1 text-xs {simMode === m
									? 'bg-neutral-800 text-neutral-100'
									: 'text-neutral-400 hover:text-neutral-200'}"
								onclick={() => (simMode = m as typeof simMode)}>{label}</button
							>
						{/each}
					</div>
					<div class="flex items-center gap-2 border-b border-neutral-800 px-3 py-2 text-xs">
						<label class="flex items-center gap-1 text-neutral-500">
							entry
							<input
								class="w-32 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-neutral-200"
								bind:value={pastedEntry}
							/>
						</label>
						<label class="flex items-center gap-1 text-neutral-500">
							ref
							<input
								class="w-24 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-neutral-200"
								placeholder="HEAD"
								bind:value={pastedBase}
							/>
						</label>
						<button
							class="ml-auto rounded bg-sky-700 px-3 py-1 font-medium text-white hover:bg-sky-600 disabled:opacity-50"
							onclick={runPaste}
							disabled={loading}>{loading ? '…' : 'Run'}</button
						>
					</div>
					<textarea
						class="thin-scroll min-h-0 flex-1 resize-none bg-neutral-950 p-3 font-mono text-xs text-neutral-300 focus:outline-none"
						placeholder="Paste a unified diff…"
						bind:value={pastedDiff}
					></textarea>
				</div>
				<div class="thin-scroll min-h-0 overflow-auto bg-neutral-900/20 p-3">
					{#if pasteResult}
						<pre class="font-mono text-xs whitespace-pre-wrap text-neutral-300">{prettyPaste}</pre>
					{:else}
						<div class="text-sm text-neutral-500">Kết quả phân tích hiện ở đây.</div>
					{/if}
				</div>
			</div>
		{/if}
	</div>
{/if}
