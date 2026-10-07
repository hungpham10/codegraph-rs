<script lang="ts">
	let { code, title = 'Diagram' }: { code: string; title?: string } = $props();

	let host: HTMLDivElement | undefined = $state();
	let error: string | null = $state(null);
	let rendered = $state(false);

	// mermaid nặng (~1MB) — lazy import, chỉ tải khi có diagram.
	$effect(() => {
		const src = code;
		if (!host || !src) return;
		let cancelled = false;
		rendered = false;
		error = null;

		(async () => {
			try {
				const mermaid = (await import('mermaid')).default;
				mermaid.initialize({
					startOnLoad: false,
					securityLevel: 'strict',
					theme: 'dark',
					fontFamily: 'ui-monospace, monospace'
				});
				const id = `m-${Math.random().toString(36).slice(2)}`;
				const { svg } = await mermaid.render(id, src);
				if (cancelled) return;
				host!.innerHTML = svg;
				rendered = true;
			} catch (e) {
				if (!cancelled) error = e instanceof Error ? e.message : String(e);
			}
		})();

		return () => {
			cancelled = true;
		};
	});
</script>

<div class="flex h-full flex-col">
	<div class="flex items-center justify-between border-b border-neutral-800 px-3 py-1.5">
		<span class="text-xs font-medium tracking-wide text-neutral-400 uppercase">{title}</span>
		<button
			class="rounded px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
			onclick={() => navigator.clipboard.writeText(code)}
		>
			Copy
		</button>
	</div>
	<div class="thin-scroll min-h-0 flex-1 overflow-auto p-4">
		{#if error}
			<div class="rounded border border-red-900 bg-red-950/40 p-3 text-sm text-red-300">
				Mermaid error: {error}
				<pre class="mt-2 text-xs whitespace-pre-wrap text-red-400/70">{code}</pre>
			</div>
		{/if}
		<div class="mermaid-host {rendered ? '' : 'hidden'}" bind:this={host}></div>
		{#if !rendered && !error}
			<div class="text-sm text-neutral-500">Rendering diagram…</div>
		{/if}
	</div>
</div>
