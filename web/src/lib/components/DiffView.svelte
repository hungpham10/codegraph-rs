<script lang="ts">
	let { diff }: { diff: string } = $props();

	type Line = { text: string; cls: string };

	let lines = $derived.by<Line[]>(() =>
		(diff ?? '').split('\n').map((text) => {
			let cls = 'text-neutral-400';
			if (text.startsWith('+++') || text.startsWith('---')) cls = 'text-neutral-500 font-semibold';
			else if (text.startsWith('@@')) cls = 'text-cyan-400';
			else if (text.startsWith('diff --git') || text.startsWith('index '))
				cls = 'text-neutral-600';
			else if (text.startsWith('+')) cls = 'bg-emerald-950/40 text-emerald-300';
			else if (text.startsWith('-')) cls = 'bg-red-950/40 text-red-300';
			return { text, cls };
		})
	);
</script>

<div class="thin-scroll h-full overflow-auto bg-neutral-950 font-mono text-xs leading-5">
	{#each lines as l, i (i)}
		<div class="px-3 {l.cls} whitespace-pre">{l.text || ' '}</div>
	{/each}
</div>
