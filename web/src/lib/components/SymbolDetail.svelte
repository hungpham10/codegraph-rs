<script lang="ts">
	import { QS } from '#lib/graphql/queries';
	import type { Symbol, FlowResult, ClassInfo, FunctionScope } from '#lib/graphql/queries';
	import { gql, gqlError } from '#lib/graphql/client';
	import { kindColor, shortPath, EFFECT_LABELS } from '#lib/format';
	import { renderMarkdown } from '#lib/markdown';
	import MermaidView from './MermaidView.svelte';
	import RelationshipList from './RelationshipList.svelte';

	let { symbol, onSelect }: { symbol: Symbol; onSelect: (s: Symbol) => void } = $props();

	type Tab = 'graph' | 'flow' | 'context' | 'structure';
	let tab = $state<Tab>('graph');
	let relCollapsed = $state(false);

	// ── Mermaid graph ──
	let kind = $state('FLOW');
	let depth = $state(2);
	let diagram = $state<string | null>(null);
	let diagramErr = $state<string | null>(null);
	let diagramLoading = $state(false);

	// ── Lists ──
	let callers = $state<Symbol[]>([]);
	let callees = $state<Symbol[]>([]);
	let impact = $state<Symbol[]>([]);

	// ── Flow detail ──
	let flow = $state<FlowResult | null>(null);

	// ── Context ──
	let context = $state<string | null>(null);
	let includeSource = $state(false);

	// ── Structure (class / scope) ──
	let classInfo = $state<ClassInfo | null>(null);
	let scope = $state<FunctionScope | null>(null);

	// Re-load mọi thứ khi symbol đổi.
	$effect(() => {
		const id = symbol.id;
		void id;
		// reset
		diagram = null;
		diagramErr = null;
		callers = [];
		callees = [];
		impact = [];
		flow = null;
		context = null;
		classInfo = null;
		scope = null;
		loadDiagram();
		loadLists();
		loadStructure();
	});

	async function loadDiagram() {
		diagramLoading = true;
		diagramErr = null;
		try {
			const data = await gql<{ mermaid: string }>(QS.mermaid, {
				id: symbol.id,
				kind,
				depth: kind === 'FLOW' ? null : depth
			});
			diagram = data.mermaid;
		} catch (e) {
			diagramErr = gqlError(e);
			diagram = null;
		} finally {
			diagramLoading = false;
		}
	}

	async function loadLists() {
		try {
			const [c, ce, im] = await Promise.all([
				gql<{ callers: Symbol[] }>(QS.callers, { id: symbol.id, depth: 1 }),
				gql<{ callees: Symbol[] }>(QS.callees, { id: symbol.id }),
				gql<{ impact: Symbol[] }>(QS.impact, { id: symbol.id, maxDepth: 3 })
			]);
			callers = c.callers;
			callees = ce.callees;
			impact = im.impact;
		} catch (e) {
			diagramErr ??= gqlError(e);
		}
	}

	async function loadFlow() {
		if (flow) return;
		try {
			const data = await gql<{ flow: FlowResult | null }>(QS.flow, { id: symbol.id });
			flow = data.flow;
		} catch (e) {
			diagramErr = gqlError(e);
		}
	}

	async function loadContext() {
		context = null;
		try {
			const data = await gql<{ context: string }>(QS.context, {
				req: { query: symbol.name, depth: 1, includeSource, limit: 5 }
			});
			context = data.context;
		} catch (e) {
			context = `Error: ${gqlError(e)}`;
		}
	}

	async function loadStructure() {
		try {
			const [ci, sc] = await Promise.all([
				gql<{ graphcodeClass: ClassInfo | null }>(QS.classInfo, { id: symbol.id }),
				gql<{ graphcodeFunctionScope: FunctionScope | null }>(QS.functionScope, {
					id: symbol.id
				})
			]);
			classInfo = ci.graphcodeClass;
			scope = sc.graphcodeFunctionScope;
		} catch {
			// không phải class/function — bỏ qua
		}
	}

	function pickKind(k: string) {
		kind = k;
		loadDiagram();
	}

	/** Chọn một row (caller/callee/member) → resolve id rồi mở. */
	async function resolveSelect(row: { id: string }) {
		try {
			const d = await gql<{ symbol: Symbol | null }>(QS.symbol, { id: row.id });
			if (d.symbol) onSelect(d.symbol);
		} catch {
			// bỏ qua
		}
	}
</script>

<div class="flex h-full flex-col">
	<!-- Header -->
	<div class="border-b border-neutral-800 bg-neutral-900/40 px-4 py-3">
		<div class="flex items-center gap-2">
			<span class="rounded border px-1.5 py-0.5 font-mono text-[11px] {kindColor(symbol.kind)}"
				>{symbol.kind}</span
			>
			<h2 class="font-mono text-lg font-semibold text-neutral-100">{symbol.name}</h2>
			{#if symbol.language}
				<span class="text-xs text-neutral-500">{symbol.language}</span>
			{/if}
		</div>
		<div class="mt-1 font-mono text-xs text-neutral-500">
			{shortPath(symbol.file, 90)}:{symbol.line}{symbol.endLine
				? `–${symbol.endLine}`
				: ''}
		</div>
		{#if symbol.signature}
			<pre
				class="mt-2 overflow-x-auto rounded bg-neutral-950 px-2 py-1.5 font-mono text-xs text-neutral-300">{symbol.signature}</pre>
		{/if}
		{#if symbol.annotations?.length}
			<div class="mt-2 flex flex-wrap gap-1">
				{#each symbol.annotations as a (a.name + a.line)}
					<span class="rounded bg-amber-950/50 px-1.5 py-0.5 font-mono text-[11px] text-amber-300"
						>@{a.name}</span
					>
				{/each}
			</div>
		{/if}
		{#if symbol.doc}
			<p class="mt-2 text-sm whitespace-pre-wrap text-neutral-400">{symbol.doc}</p>
		{/if}
	</div>

	<!-- Tabs -->
	<div class="flex items-center gap-1 border-b border-neutral-800 px-2 py-1">
		{#each [['graph', 'Graph'], ['flow', 'Flow'], ['structure', 'Structure'], ['context', 'Context']] as [id, label] (id)}
			<button
				class="rounded px-3 py-1 text-xs font-medium {tab === id
					? 'bg-neutral-800 text-neutral-100'
					: 'text-neutral-400 hover:text-neutral-200'}"
				onclick={() => {
					tab = id as Tab;
					if (id === 'flow') loadFlow();
					if (id === 'context') loadContext();
				}}
			>
				{label}
			</button>
		{/each}
		<div class="ml-auto flex items-center gap-1 text-xs">
			{#each ['FLOW', 'CALLERS', 'CALLEES', 'IMPACT'] as k (k)}
				<button
					class="rounded px-2 py-0.5 {kind === k
						? 'bg-sky-900/50 text-sky-300'
						: 'text-neutral-500 hover:text-neutral-300'}"
					onclick={() => {
						tab = 'graph';
						pickKind(k);
					}}>{k.toLowerCase()}</button
				>
			{/each}
			{#if kind !== 'FLOW'}
				<label class="ml-1 flex items-center gap-1 text-neutral-500">
					depth
					<input
						type="number"
						min="1"
						max="6"
						class="w-10 rounded border border-neutral-700 bg-neutral-950 px-1 text-center text-neutral-200"
						bind:value={depth}
						onchange={loadDiagram}
					/>
				</label>
			{/if}
		</div>
	</div>

	<!-- Body -->
	<div class="min-h-0 flex-1 overflow-hidden">
		{#if tab === 'graph'}
			<div class="flex h-full">
				<div class="min-w-0 flex-1">
					{#if diagramLoading}
						<div class="p-4 text-sm text-neutral-500">Rendering…</div>
					{:else if diagramErr}
						<div class="m-4 rounded border border-red-900 bg-red-950/40 p-3 text-sm text-red-300">
							{diagramErr}
						</div>
					{:else if diagram}
						<MermaidView code={diagram} title="{kind} · {symbol.name}" />
					{/if}
				</div>

				{#if relCollapsed}
					<button
						class="w-6 shrink-0 border-l border-neutral-800 text-neutral-600 hover:bg-neutral-800/60 hover:text-neutral-300"
						title="Show relationships"
						onclick={() => (relCollapsed = false)}>‹</button
					>
				{:else}
					<div class="flex w-80 shrink-0 flex-col border-l border-neutral-800">
						<div class="flex items-center justify-end border-b border-neutral-800 px-2 py-1">
							<button
								class="rounded px-1.5 py-0.5 text-xs text-neutral-500 hover:bg-neutral-800 hover:text-neutral-300"
								title="Hide relationships"
								onclick={() => (relCollapsed = true)}>›</button
							>
						</div>
						<div class="thin-scroll min-h-0 flex-1 overflow-auto p-3">
							<RelationshipList title="Callers" items={callers} onSelect={resolveSelect} />
							<RelationshipList title="Callees" items={callees} onSelect={resolveSelect} />
							<RelationshipList title="Impact" items={impact} onSelect={resolveSelect} />
						</div>
					</div>
				{/if}
			</div>
		{:else if tab === 'flow'}
			<div class="thin-scroll h-full overflow-auto p-4">
				{#if !flow}
					<div class="text-sm text-neutral-500">Loading flow…</div>
				{:else}
					<div class="mb-3 font-mono text-xs text-neutral-500">{flow.chainDesc.length} steps · {flow.calls.length} calls</div>
					<ol class="flex flex-col gap-1">
						{#each flow.chainDesc as d, i (i)}
							<li class="flex items-start gap-2 font-mono text-xs">
								<span class="w-8 shrink-0 text-right text-neutral-600">{i}</span>
								<span class="text-neutral-300">{d || '·'}</span>
							</li>
						{/each}
					</ol>
					{#if flow.calls.length}
						<h3 class="mt-4 mb-2 text-xs font-semibold tracking-wide text-neutral-400 uppercase">
							Calls
						</h3>
						<div class="flex flex-col gap-1">
							{#each flow.calls as c (c.position)}
								<div class="flex items-center gap-2 rounded bg-neutral-900/60 px-2 py-1 text-xs">
									{#if c.toId}
										<button
											class="font-mono text-sky-300 hover:underline"
											onclick={async () => {
												const d = await gql<{ symbol: Symbol | null }>(QS.symbol, { id: c.toId });
												if (d.symbol) onSelect(d.symbol);
											}}>{c.toName}</button
										>
									{:else}
										<span class="font-mono text-neutral-300">{c.toName}</span>
										<span class="text-[10px] text-neutral-600">ext</span>
									{/if}
									<span class="text-neutral-600">L{c.line}</span>
									{#if c.condition}<span class="truncate text-amber-400/80">if {c.condition}</span>{/if}
									{#if EFFECT_LABELS[c.effect]}
										<span class="rounded bg-neutral-800 px-1 text-[10px] text-neutral-400"
											>{EFFECT_LABELS[c.effect]}</span
										>
									{/if}
								</div>
							{/each}
						</div>
					{/if}
				{/if}
			</div>
		{:else if tab === 'structure'}
			<div class="thin-scroll h-full overflow-auto p-4">
				{#if classInfo}
					<h3 class="mb-2 text-xs font-semibold tracking-wide text-neutral-400 uppercase">
						Methods
					</h3>
					<RelationshipList title="" items={classInfo.methods} onSelect={resolveSelect} />
					<h3 class="mt-4 mb-2 text-xs font-semibold tracking-wide text-neutral-400 uppercase">
						Fields
					</h3>
					<RelationshipList title="" items={classInfo.fields} onSelect={resolveSelect} />
				{/if}
				{#if scope}
					<h3 class="mt-4 mb-2 text-xs font-semibold tracking-wide text-neutral-400 uppercase">
						Parameters
					</h3>
					<RelationshipList title="" items={scope.parameters} onSelect={resolveSelect} />
					<h3 class="mt-4 mb-2 text-xs font-semibold tracking-wide text-neutral-400 uppercase">
						Locals
					</h3>
					<RelationshipList title="" items={scope.locals} onSelect={resolveSelect} />
				{/if}
				{#if !classInfo && !scope}
					<div class="text-sm text-neutral-500">No structure info for this symbol.</div>
				{/if}
			</div>
		{:else if tab === 'context'}
			<div class="flex h-full flex-col">
				<div class="flex items-center gap-2 border-b border-neutral-800 px-3 py-1.5 text-xs text-neutral-400">
					<label class="flex items-center gap-1">
						<input type="checkbox" bind:checked={includeSource} onchange={loadContext} />
						include source
					</label>
				</div>
				<div class="thin-scroll min-h-0 flex-1 overflow-auto p-4">
					{#if context}
						<article class="prose prose-invert prose-sm max-w-none prose-pre:bg-neutral-950">
							{@html renderMarkdown(context)}
						</article>
					{:else}
						<div class="text-sm text-neutral-500">Loading context…</div>
					{/if}
				</div>
			</div>
		{/if}
	</div>
</div>
