<script lang="ts">
	let { code, title = 'Diagram' }: { code: string; title?: string } = $props();

	let viewport: HTMLDivElement | undefined = $state();
	let host: HTMLDivElement | undefined = $state();
	let error: string | null = $state(null);
	let rendered = $state(false);

	// ── View transform ──
	let scale = $state(1);
	let tx = $state(0);
	let ty = $state(0);
	let diagramW = $state(0);
	let diagramH = $state(0);
	let dragging = $state(false);
	let lastX = 0;
	let lastY = 0;

	const MIN_SCALE = 0.1;
	const MAX_SCALE = 10;

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
				const el = host!.querySelector('svg') as SVGSVGElement | null;
				const vb = el?.viewBox?.baseVal;
				diagramW = vb?.width || el?.getBoundingClientRect().width || 800;
				diagramH = vb?.height || el?.getBoundingClientRect().height || 600;

				rendered = true;
				fit(); // canh vừa khung lần đầu
			} catch (e) {
				if (!cancelled) error = e instanceof Error ? e.message : String(e);
			}
		})();

		return () => {
			cancelled = true;
		};
	});

	// Wheel = zoom tại con trỏ. Phải addEventListener thủ công để set passive:false.
	$effect(() => {
		const vp = viewport;
		if (!vp) return;
		const onWheel = (e: WheelEvent) => {
			e.preventDefault();
			const rect = vp.getBoundingClientRect();
			const factor = Math.exp(-e.deltaY * 0.0015);
			zoomAt(scale * factor, e.clientX - rect.left, e.clientY - rect.top);
		};
		vp.addEventListener('wheel', onWheel, { passive: false });
		return () => vp.removeEventListener('wheel', onWheel);
	});

	/** scale mới, giữ điểm (mx,my) trong viewport cố định. */
	function zoomAt(next: number, mx: number, my: number) {
		const s = Math.min(MAX_SCALE, Math.max(MIN_SCALE, next));
		if (s === scale) return;
		const k = s / scale;
		tx = mx - (mx - tx) * k;
		ty = my - (my - ty) * k;
		scale = s;
	}

	function zoomBy(factor: number) {
		if (!viewport) return;
		zoomAt(scale * factor, viewport.clientWidth / 2, viewport.clientHeight / 2);
	}

	/** Vừa khung (có padding). */
	function fit() {
		if (!viewport || !diagramW || !diagramH) return;
		const pad = 24;
		const s = Math.max(
			MIN_SCALE,
			Math.min((viewport.clientWidth - pad * 2) / diagramW, (viewport.clientHeight - pad * 2) / diagramH)
		);
		scale = s;
		tx = (viewport.clientWidth - diagramW * s) / 2;
		ty = (viewport.clientHeight - diagramH * s) / 2;
	}

	function reset100() {
		if (!viewport) return;
		scale = 1;
		tx = (viewport.clientWidth - diagramW) / 2;
		ty = (viewport.clientHeight - diagramH) / 2;
	}

	// Pan bằng kéo chuột.
	function onPointerDown(e: PointerEvent) {
		if (e.button !== 0 || !viewport) return;
		dragging = true;
		lastX = e.clientX;
		lastY = e.clientY;
		viewport.setPointerCapture(e.pointerId);
	}
	function onPointerMove(e: PointerEvent) {
		if (!dragging) return;
		tx += e.clientX - lastX;
		ty += e.clientY - lastY;
		lastX = e.clientX;
		lastY = e.clientY;
	}
	function onPointerUp(e: PointerEvent) {
		dragging = false;
		try {
			viewport?.releasePointerCapture(e.pointerId);
		} catch {
			/* ignore */
		}
	}
</script>

<div class="flex h-full flex-col">
	<div class="flex items-center justify-between border-b border-neutral-800 px-3 py-1.5">
		<span class="text-xs font-medium tracking-wide text-neutral-400 uppercase">{title}</span>

		<div class="flex items-center gap-1">
			<button class="rounded px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
				title="Zoom out" onclick={() => zoomBy(1 / 1.25)}>−</button>
			<span class="w-12 text-center font-mono text-[11px] text-neutral-500">{Math.round(scale * 100)}%</span>
			<button class="rounded px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
				title="Zoom in" onclick={() => zoomBy(1.25)}>+</button>
			<button class="rounded px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
				title="Fit to view" onclick={fit}>⤢ Fit</button>
			<button class="rounded px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
				title="Actual size" onclick={reset100}>100%</button>
			<button class="ml-1 rounded px-2 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
				onclick={() => navigator.clipboard.writeText(code)}>Copy</button>
		</div>
	</div>

	{#if error}
		<div class="m-4 rounded border border-red-900 bg-red-950/40 p-3 text-sm text-red-300">
			Mermaid error: {error}
			<pre class="mt-2 text-xs whitespace-pre-wrap text-red-400/70">{code}</pre>
		</div>
	{:else}
		<!-- viewport: bắt zoom + pan -->
		<div
			role="application"
			aria-label="Diagram viewport"
			class="relative min-h-0 flex-1 overflow-hidden {dragging ? 'cursor-grabbing select-none' : 'cursor-grab'}"
			bind:this={viewport}
			onpointerdown={onPointerDown}
			onpointermove={onPointerMove}
			onpointerup={onPointerUp}
			onpointercancel={onPointerUp}
			ondblclick={fit}
		>
			<!-- stage: transform scale/translate -->
			<div
				class="mermaid-host absolute top-0 left-0 origin-top-left"
				bind:this={host}
				style="width:{diagramW}px;height:{diagramH}px;transform:translate({tx}px,{ty}px) scale({scale});"
			></div>

			{#if !rendered}
				<div class="absolute inset-0 flex items-center justify-center text-sm text-neutral-500">
					Rendering diagram…
				</div>
			{/if}
		</div>
	{/if}
</div>