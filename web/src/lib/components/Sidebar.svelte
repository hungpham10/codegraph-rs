<script lang="ts">
	import { page } from '$app/state';
	import { workspace } from '#lib/workspace.svelte';

	const ALL_ITEMS = [
		{ id: 'explore', href: '/', label: 'Explore', hint: 'symbol · call graph · flow', icon: '⌕' },
		{ id: 'review', href: '/review', label: 'Review', hint: 'diff · impact', icon: '⇄' },
		{ id: 'documents', href: '/documents', label: 'Documents', hint: 'yaml · json · toml · hcl', icon: '▤' }
	];

	let current = $derived(page.url.pathname);
	let collapsed = $state(false);

	// Chỉ giữ module được bật theo config server.
	let items = $derived(ALL_ITEMS.filter((i) => workspace.hasModule(i.id)));
</script>

{#if items.length > 0}
	<nav
		class="flex shrink-0 flex-col gap-1 border-r border-neutral-800 bg-neutral-900/40 p-2 transition-[width] {collapsed
			? 'w-12'
			: 'w-56'}"
	>
		{#each items as item (item.id)}
			{@const active = current === item.href}
			<a
				href={item.href}
				title={collapsed ? `${item.label} — ${item.hint}` : undefined}
				class="flex items-center gap-2 rounded-md px-2 py-2 transition-colors {active
					? 'bg-neutral-800 text-neutral-100'
					: 'text-neutral-400 hover:bg-neutral-800/60 hover:text-neutral-200'}"
			>
				<span class="w-5 shrink-0 text-center text-sm">{item.icon}</span>
				{#if !collapsed}
					<span class="flex flex-col">
						<span class="text-sm font-medium">{item.label}</span>
						<span class="text-[11px] text-neutral-500">{item.hint}</span>
					</span>
				{/if}
			</a>
		{/each}

		<div class="mt-auto flex flex-col gap-1">
			{#if !collapsed}
				<div class="px-2 text-[11px] leading-relaxed text-neutral-600">
					Backend: <code class="text-neutral-500">codegraph serve --graphql --mermaid</code>
				</div>
			{/if}
			<button
				class="flex items-center gap-2 rounded-md px-2 py-1.5 text-neutral-500 hover:bg-neutral-800/60 hover:text-neutral-300"
				title={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
				onclick={() => (collapsed = !collapsed)}
			>
				<span class="w-5 shrink-0 text-center text-sm">{collapsed ? '›' : '‹'}</span>
				{#if !collapsed}<span class="text-xs">collapse</span>{/if}
			</button>
		</div>
	</nav>
{/if}
