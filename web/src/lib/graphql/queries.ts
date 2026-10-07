// GraphQL documents + kiểu dữ liệu trả về của codegraph API.
// Enum async-graphql dùng SCREAMING_SNAKE_CASE (SymbolKind, ScopeLevel, …).

export interface Annotation {
	name: string;
	line: number;
	args: Record<string, string> | null;
}

export interface Symbol {
	id: string;
	name: string;
	kind: string;
	scope: string;
	scopeId: string;
	typeRef: string;
	typeName: string | null;
	file: string;
	line: number;
	endLine: number;
	signature: string | null;
	doc: string | null;
	annotations: Annotation[];
	language: string;
}

export interface SearchSymbolResult {
	symbols: Symbol[];
	total: number;
	timedOut: boolean;
	resume: string | null;
	indexVersion: number;
}

export interface FileInfo {
	path: string;
	language: string;
	bytes: number;
	lines: number;
}

export interface Status {
	symbols: number;
	chains: number;
	edges: number;
	files: number;
	nextId: number;
}

export interface MemberInfo {
	id: string;
	name: string;
	kind: string;
	line: number;
	signature: string | null;
}

export interface ClassInfo {
	class: Symbol;
	fields: MemberInfo[];
	methods: MemberInfo[];
}

export interface FunctionScope {
	function: Symbol;
	parameters: Symbol[];
	locals: Symbol[];
}

export interface Dependency {
	name: string;
	count: number;
}

export interface DependenciesReport {
	internal: Dependency[];
	external: Dependency[];
	total: number;
}

export interface FlowCall {
	position: number;
	toName: string;
	toId: string | null;
	line: number;
	condition: string | null;
	effect: string;
	effectDesc: string | null;
	args: string[];
}

export interface FlowResult {
	symbol: Symbol;
	chain: string[];
	chainDesc: string[];
	calls: FlowCall[];
}

export interface SearchFlowResult {
	functionId: string;
	functionName: string;
	chain: string[];
	matchCount: number;
}

export interface CallSite {
	callerId: string;
	callName: string;
	line: number;
	condition: string | null;
	isLoopBody: boolean;
	argExprs: string[];
}

export interface CallSiteResult {
	funcId: string;
	funcName: string;
	file: string;
	callSites: CallSite[];
}

export interface ListResult {
	items: Symbol[];
	total: number;
	hasMore: boolean;
}

export interface DocInfo {
	docId: string;
	path: string;
	format: string;
	rootNodeId: string;
	nodes: number;
}

export interface DocNode {
	id: string;
	path: string[];
	kind: string;
	value: string | null;
	key: string | null;
	doc: string;
	children: DocNode[];
}

export interface DocStats {
	docs: number;
	nodes: number;
}

export interface DocPattern {
	patternId: string;
	tokens: string[];
	nodeCount: number;
	docCount: number;
	docFreq: number;
}

const SYMBOL_FIELDS = `
	id name kind scope scopeId typeRef typeName file line endLine
	signature doc language
	annotations { name line args }
`;

// ── Query lookups ──

export const QS = {
	status: `query { status { symbols chains edges files nextId } }`,

	uiConfig: `query { uiConfig }`,

	searchSymbol: `
		query Search($input: SearchSymbolInput!) {
			searchSymbol(input: $input) {
				symbols { ${SYMBOL_FIELDS} }
				total timedOut resume indexVersion
			}
		}`,

	symbol: `
		query Symbol($id: ID) {
			symbol(id: $id) { ${SYMBOL_FIELDS} }
		}`,

	callers: `
		query Callers($id: ID!, $depth: Int) {
			callers(id: $id, depth: $depth) { ${SYMBOL_FIELDS} }
		}`,

	callees: `
		query Callees($id: ID!) {
			callees(id: $id) { ${SYMBOL_FIELDS} }
		}`,

	impact: `
		query Impact($id: ID!, $maxDepth: Int) {
			impact(id: $id, maxDepth: $maxDepth) { ${SYMBOL_FIELDS} }
		}`,

	flow: `
		query Flow($id: ID!) {
			flow(id: $id) {
				symbol { ${SYMBOL_FIELDS} }
				chain chainDesc
				calls { position toName toId line condition effect effectDesc args }
			}
		}`,

	mermaid: `
		query Mermaid($id: ID!, $kind: MermaidKind!, $depth: Int) {
			mermaid(id: $id, kind: $kind, depth: $depth)
		}`,

	context: `
		query Context($req: ContextRequestInput!) {
			context(req: $req)
		}`,

	files: `
		query Files($prefix: String) {
			graphcodeFiles(prefix: $prefix) { path language bytes lines }
		}`,

	classInfo: `
		query Class($id: ID!) {
			graphcodeClass(id: $id) {
				class { ${SYMBOL_FIELDS} }
				fields { id name kind line signature }
				methods { id name kind line signature }
			}
		}`,

	functionScope: `
		query Scope($id: ID!) {
			graphcodeFunctionScope(id: $id) {
				function { ${SYMBOL_FIELDS} }
				parameters { ${SYMBOL_FIELDS} }
				locals { ${SYMBOL_FIELDS} }
			}
		}`,

	listTypes: `
		query Types($kind: TypeKind!, $limit: Int, $offset: Int) {
			graphcodeListTypes(kind: $kind, limit: $limit, offset: $offset) {
				items { ${SYMBOL_FIELDS} }
				total hasMore
			}
		}`,

	listSymbols: `
		query ListSymbols($kind: SymbolKind!, $limit: Int, $offset: Int) {
			graphcodeListSymbols(kind: $kind, limit: $limit, offset: $offset) {
				items { ${SYMBOL_FIELDS} }
				total hasMore
			}
		}`,

	searchFlow: `
		query SearchFlow($pattern: String!, $limit: Int, $offset: Int) {
			searchFlow(pattern: $pattern, limit: $limit, offset: $offset) {
				results { functionId functionName chain matchCount }
				total hasMore
			}
		}`,

	references: `
		query References($query: String!, $limit: Int) {
			references(query: $query, limit: $limit) {
				results {
					funcId funcName file
					callSites { callerId callName line condition isLoopBody argExprs }
				}
				total hasMore
			}
		}`,

	dependencies: `
		query Deps {
			graphcodeDependencies {
				internal { name count }
				external { name count }
				total
			}
		}`,

	gitBranches: `query { gitBranches }`,

	// ── Documents ──
	docStats: `query { graphdocStats { docs nodes } }`,

	docList: `
		query {
			graphdocList { docId path format rootNodeId nodes }
		}`,

	docSearch: `
		query DocSearch($pattern: String!, $depth: Int) {
			graphdocSearch(pattern: $pattern, depth: $depth) {
				id path kind value key doc children { id path kind }
			}
		}`,

	docHydrate: `
		query DocHydrate($id: ID!, $depth: Int) {
			graphdocHydrate(id: $id, maxDepth: $depth) {
				id path kind value key doc
				children {
					id path kind value key
					children { id path kind value key }
				}
			}
		}`,

	docSearchValue: `
		query DocValue($query: String!, $limit: Int) {
			graphdocSearchValue(query: $query, limit: $limit) {
				id path kind value key doc
			}
		}`,

	docPatterns: `
		query {
			graphdocListPatterns { patternId tokens nodeCount docCount docFreq }
		}`
};

// ── Mutations ──

export const MS = {
	init: `
		mutation Init($path: String!, $index: Boolean) {
			init(path: $path, index: $index)
		}`,
	deinit: `mutation { deinit }`,
	index: `mutation { index }`,
	diff: `
		mutation Diff($args: JSON!) {
			graphcodeDiff(args: $args)
		}`,
	diffSimulate: `
		mutation DiffSim($args: JSON!) {
			graphcodeDiffSimulate(args: $args)
		}`,
	originSimulate: `
		mutation OriginSim($args: JSON!) {
			graphcodeOriginSimulate(args: $args)
		}`,
	branchCompare: `
		mutation BranchCompare($args: JSON!) {
			graphcodeBranchCompare(args: $args)
		}`,

	// ── Documents ──
	docIngest: `
		mutation DocIngest($path: String!, $format: String) {
			graphdocIngest(path: $path, format: $format)
		}`,
	docIngestDir: `
		mutation DocIngestDir($path: String!, $limit: Int) {
			graphdocIngestDir(path: $path, limit: $limit)
		}`,
	docRemove: `
		mutation DocRemove($id: ID!) {
			graphdocRemove(id: $id)
		}`,
	docMinePatterns: `
		mutation DocMine($topK: Int, $minCount: Int, $maxDepth: Int) {
			graphdocMinePatterns(topK: $topK, minCount: $minCount, maxDepth: $maxDepth)
		}`
};
