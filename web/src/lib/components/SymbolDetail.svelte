<script lang="ts">
	import { QS } from '#lib/graphql/queries';
	import type {
	Symbol,
	FlowResult,
	ClassInfo,
	FunctionScope,
	FullFlowPath,
	SearchFlowResult,
	SearchSymbolResult
} from '#lib/graphql/queries';
	import { gql, gqlError } from '#lib/graphql/client';
	import { kindColor, shortPath, EFFECT_LABELS } from '#lib/format';
	import { renderMarkdown } from '#lib/markdown';
	import MermaidView from './MermaidView.svelte';
	import RelationshipList from './RelationshipList.svelte';

	let { symbol, onSelect }: { symbol: Symbol; onSelect: (s: Symbol) => void } = $props();

	// Dynamic dispatch badge theo ResolutionKind (backend semgraph).
	// GraphQL enum trả SCREAMING_SNAKE_CASE.
	const DYN_BADGES: Record<string, { label: string; cls: string; title: string }> = {
		HIERARCHY_FAN_OUT: {
			label: '[dyn]',
			cls: 'bg-emerald-900/60 text-emerald-300',
			title: 'Dynamic dispatch — fan-out qua trait/interface/base hierarchy'
		},
		POINTER_ASSIGN: {
			label: '[fn-ptr]',
			cls: 'bg-sky-900/60 text-sky-300',
			title: 'Function pointer — qua assign-site tracking'
		},
		UNRESOLVED_DYNAMIC: {
			label: '[unresolved]',
			cls: 'bg-red-900/60 text-red-300',
			title: 'Dynamic nhưng không có target trong index (reflection/monkey-patch)'
		},
		ALIAS: { label: '[alias]', cls: 'bg-neutral-800 text-neutral-400', title: 'Resolve qua alias' },
		SHORT_NAME: {
			label: '[short]',
			cls: 'bg-neutral-800 text-neutral-400',
			title: 'Resolve qua short-name fallback'
		}
	};

	type Tab = 'graph' | 'flow' | 'context' | 'structure' | 'callsites' | 'pattern' | 'trace';

	// ── Full-flow trace (debug-style paths caller → target) ──
	let tracePaths = $state<FullFlowPath[]>([]);
	let traceDepth = $state(5);
	let traceLoading = $state(false);
	let traceError = $state<string | null>(null);

	async function loadTrace() {
		traceLoading = true;
		traceError = null;
		try {
			const d = await gql<{ traceFullFlow: FullFlowPath[] }>(QS.traceFullFlow, {
				targetId: symbol.id,
				maxDepth: traceDepth,
				maxPaths: 50,
				maxLoopIter: 1
			});
			tracePaths = d.traceFullFlow;
		} catch (e) {
			traceError = gqlError(e);
			tracePaths = [];
		} finally {
			traceLoading = false;
		}
	}

	const ACTION_LABELS: Record<string, string> = {
		CALL: '→',
		RETURN: '←',
		THROW: '!',
		ASSERT: '!!',
		BRANCH: '§',
		LOOP: '↻',
		STATEMENT: '·'
	};
	const ACTION_CLS: Record<string, string> = {
		CALL: 'text-sky-300',
		RETURN: 'text-neutral-400',
		THROW: 'text-red-300',
		ASSERT: 'text-red-300',
		BRANCH: 'text-amber-300',
		LOOP: 'text-emerald-300',
		STATEMENT: 'text-neutral-600'
	};
	const ENDING_CLS: Record<string, string> = {
		RETURNED: 'text-emerald-300',
		THREW: 'text-red-300',
		ASSERTED: 'text-red-300',
		DEPTH_LIMIT: 'text-amber-300',
		PATH_LIMIT: 'text-amber-300'
	};

	// ── Call sites (references theo call name) ──
	type CallSite = {
		callerId: string;
		callName: string;
		line: number;
		condition: string | null;
		isLoopBody: boolean | null;
		argExprs: string[] | null;
	};
	type RefResult = { funcId: string; funcName: string; file: string; callSites: CallSite[] };
	let refQuery = $state('');
	let refResults = $state<RefResult[]>([]);
	let refTotal = $state(0);
	let refLoading = $state(false);
	let refError = $state<string | null>(null);

	async function loadReferences(name: string) {
		if (!name.trim()) return;
		refLoading = true;
		refError = null;
		try {
			const d = await gql<{ references: { results: RefResult[]; total: number } }>(QS.references, {
				query: name.trim(),
				limit: 50
			});
			refResults = d.references.results;
			refTotal = d.references.total;
		} catch (e) {
			refError = gqlError(e);
			refResults = [];
		} finally {
			refLoading = false;
		}
	}

	// ── Flow-pattern search ──
	let patQuery = $state('');
	let patResults = $state<SearchFlowResult[]>([]);
	let patLoading = $state(false);
	let patError = $state<string | null>(null);

	async function loadPattern(pattern: string) {
		if (!pattern.trim()) return;
		patLoading = true;
		patError = null;
		try {
			const d = await gql<{ searchFlow: { results: SearchFlowResult[]; total: number } }>(
				QS.searchFlow,
				{ pattern: pattern.trim(), limit: 50, offset: 0 }
			);
			patResults = d.searchFlow.results;
		} catch (e) {
			patError = gqlError(e);
			patResults = [];
		} finally {
			patLoading = false;
		}
	}
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
		refQuery = symbol.name;
		refResults = [];
		refTotal = 0;
		refError = null;
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

	// Tìm symbol theo exact name (dynamic candidate click).
	async function searchExact(name: string) {
		try {
			const d = await gql<{ searchSymbol: SearchSymbolResult }>(QS.searchSymbol, {
				input: { query: name, mode: 'EXACT', limit: 1 }
			});
			if (d.searchSymbol.symbols[0]) onSelect(d.searchSymbol.symbols[0]);
		} catch {
			// ignore — chip chỉ là hint
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
		{#each [['graph', 'Graph'], ['flow', 'Flow'], ['structure', 'Structure'], ['context', 'Context'], ['callsites', 'Call sites'], ['pattern', 'Pattern'], ['trace', 'Trace']] as [id, label] (id)}
			<button
				class="rounded px-3 py-1 text-xs font-medium {tab === id
					? 'bg-neutral-800 text-neutral-100'
					: 'text-neutral-400 hover:text-neutral-200'}"
				onclick={() => {
					tab = id as Tab;
					if (id === 'flow') loadFlow();
					if (id === 'context') loadContext();
					if (id === 'callsites' && !refResults.length) void loadReferences(symbol.name);
					if (id === 'trace' && !tracePaths.length && !traceLoading) void loadTrace();
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
									{#if DYN_BADGES[c.resolvedVia]}
										<span
											class="rounded px-1 text-[10px] {DYN_BADGES[c.resolvedVia].cls}"
											title={DYN_BADGES[c.resolvedVia].title}>{DYN_BADGES[c.resolvedVia].label}</span
										>
									{/if}
								</div>
								{#if c.candidateNames?.length}
									<div class="ml-10 flex flex-wrap gap-1">
										{#each c.candidateNames as cand (cand)}
											<button
												class="rounded bg-neutral-800/70 px-1.5 py-0.5 font-mono text-[10px] text-emerald-300 hover:bg-neutral-700"
												title="Dynamic candidate — search by name"
												onclick={() => searchExact(cand)}>{cand}</button
											>
										{/each}
									</div>
								{/if}
							{/each}
						</div>
					{/if}
				{/if}
			</div>
		{:else if tab === 'callsites'}
			<div class="flex h-full flex-col">
				<div class="flex gap-2 border-b border-neutral-800 px-3 py-1.5">
					<input
						class="flex-1 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-xs text-neutral-100 placeholder-neutral-600 focus:border-sky-700 focus:outline-none"
						placeholder="call name (vd fmt.Println, handler)"
						bind:value={refQuery}
						onkeydown={(e) => e.key === 'Enter' && void loadReferences(refQuery)}
					/>
					<button
						class="rounded bg-sky-700 px-3 py-1 text-xs text-white hover:bg-sky-600 disabled:opacity-50"
						disabled={refLoading}
						onclick={() => void loadReferences(refQuery)}
					>
						{refLoading ? '…' : 'Find'}
					</button>
				</div>
				<div class="thin-scroll min-h-0 flex-1 overflow-auto p-3">
					{#if refError}
						<div class="rounded border border-red-900 bg-red-950/40 p-2 text-xs text-red-300"
							>{refError}</div
						>
					{:else if refLoading}
						<div class="text-xs text-neutral-500">Searching call sites…</div>
					{:else if !refResults.length}
						<p class="mb-2 text-[11px] text-neutral-600">
							Callers theo call name — thấy cả unresolved/library calls (kể cả dynamic:
							fn-ptr, interface dispatch).
						</p>
					{:else}
						<p class="mb-2 text-[11px] text-neutral-600">{refTotal} function(s) gọi tên này.</p>
						{#each refResults as r (r.funcId)}
							<div class="mb-2 rounded border border-neutral-800">
								<div class="flex items-center gap-2 border-b border-neutral-800 bg-neutral-900/60 px-2 py-1">
									<button
										class="font-mono text-xs text-sky-300 hover:underline"
										onclick={() => void searchExact(r.funcName)}>{r.funcName}</button
									>
									<span class="truncate text-[10px] text-neutral-500">{r.file}</span>
								</div>
								<div class="flex flex-col">
									{#each r.callSites as cs, i (i)}
										<div class="flex items-center gap-2 px-2 py-0.5 font-mono text-[11px]">
											<span class="w-10 shrink-0 text-neutral-600">L{cs.line}</span>
											<span class="truncate text-neutral-300">{cs.callName}</span>
											{#if cs.isLoopBody}
												<span class="rounded bg-emerald-900/60 px-1 text-[9px] text-emerald-300"
													>loop</span
												>
											{/if}
											{#if cs.condition}
												<span class="truncate text-amber-400/80">if {cs.condition}</span>
											{/if}
										</div>
									{/each}
								</div>
							</div>
						{/each}
					{/if}
				</div>
			</div>
		{:else if tab === 'pattern'}
			<div class="flex h-full flex-col">
				<div class="flex gap-2 border-b border-neutral-800 px-3 py-1.5">
					<input
						class="flex-1 rounded border border-neutral-700 bg-neutral-950 px-2 py-1 font-mono text-xs text-neutral-100 placeholder-neutral-600 focus:border-sky-700 focus:outline-none"
						placeholder="pattern: ids/markers/names phân cách bởi dấu phẩy (vd IF_TRUE, run)"
						bind:value={patQuery}
						onkeydown={(e) => e.key === 'Enter' && void loadPattern(patQuery)}
					/>
					<button
						class="rounded bg-sky-700 px-3 py-1 text-xs text-white hover:bg-sky-600 disabled:opacity-50"
						disabled={patLoading}
						onclick={() => void loadPattern(patQuery)}
					>
						{patLoading ? '…' : 'Search'}
					</button>
				</div>
				<div class="thin-scroll min-h-0 flex-1 overflow-auto p-3">
					{#if patError}
						<div class="rounded border border-red-900 bg-red-950/40 p-2 text-xs text-red-300"
							>{patError}</div
						>
					{:else if patLoading}
						<div class="text-xs text-neutral-500">Searching flow patterns…</div>
					{:else if !patResults.length}
						<p class="text-[11px] text-neutral-600">
							Tìm hàm mà chain chứa pattern (markers: IF_TRUE, LOOP, SWITCH_CASE, RETURN…;
							tên hàm; hoặc symbol id).
						</p>
					{:else}
						<div class="flex flex-col gap-1">
							{#each patResults as r (r.functionId)}
								<button
									class="flex flex-col rounded px-2 py-1.5 text-left hover:bg-neutral-800/60"
									onclick={() => void searchExact(r.functionName)}
								>
									<div class="flex items-center gap-2">
										<span class="font-mono text-xs text-neutral-100">{r.functionName}</span>
										<span class="rounded bg-neutral-800 px-1 text-[10px] text-neutral-400"
											>{r.matchCount} match</span
										>
									</div>
									<span class="truncate font-mono text-[10px] text-neutral-500"
										>{r.chain.slice(0, 24).join(' → ')}</span
									>
								</button>
							{/each}
						</div>
					{/if}
				</div>
			</div>
		{:else if tab === 'trace'}
			<div class="flex h-full flex-col">
				<div class="flex items-center gap-2 border-b border-neutral-800 px-3 py-1.5 text-xs text-neutral-400">
					<span>caller depth</span>
					<input
						type="number"
						min="1"
						max="10"
						class="w-14 rounded border border-neutral-700 bg-neutral-950 px-1.5 py-0.5 text-xs text-neutral-200"
						bind:value={traceDepth}
						onchange={() => void loadTrace()}
					/>
					<span class="text-neutral-600">
						{tracePaths.length} path(s) từ callers → {symbol.name}
					</span>
				</div>
				<div class="thin-scroll min-h-0 flex-1 overflow-auto p-3">
					{#if traceError}
						<div class="rounded border border-red-900 bg-red-950/40 p-2 text-xs text-red-300"
							>{traceError}</div
						>
					{:else if traceLoading}
						<div class="text-xs text-neutral-500">Tracing full flow…</div>
					{:else if !tracePaths.length}
						<p class="text-[11px] text-neutral-600">
							Debug-style path: mọi tuyến từ caller xuống {symbol.name} kèm call/return/branch.
						</p>
					{:else}
						<div class="flex flex-col gap-3">
							{#each tracePaths as path, pi (pi)}
								<div class="rounded border border-neutral-800">
									<div class="border-b border-neutral-800 bg-neutral-900/60 px-2 py-1 text-[11px]">
										<span class="font-mono text-neutral-400">path #{pi + 1}</span>
										<span class="ml-2 {ENDING_CLS[path.endedWith] ?? 'text-neutral-400'}"
											>{path.endedWith.toLowerCase()}</span
										>
									</div>
									<div class="flex flex-col py-1">
										{#each path.steps as st, si (si)}
											<div
												class="flex items-center gap-2 px-2 py-0.5 font-mono text-[11px]"
												style="padding-left: {8 + st.stackDepth * 14}px"
											>
												<span class="w-4 shrink-0 {ACTION_CLS[st.action] ?? 'text-neutral-400'}"
													>{ACTION_LABELS[st.action] ?? '·'}</span
												>
												{#if st.action === 'CALL'}
													<button
														class="text-sky-300 hover:underline"
														onclick={() => void searchExact(st.symbolName)}
														>{st.symbolName}</button
													>
												{:else}
													<span class="text-neutral-300">{st.symbolName}</span>
												{/if}
												{#if st.line}
													<span class="text-neutral-600">L{st.line}</span>
												{/if}
												{#if st.condition}
													<span class="truncate text-amber-400/80">if {st.condition}</span>
												{/if}
											</div>
										{/each}
									</div>
								</div>
							{/each}
						</div>
					{/if}
				</div>
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
