<script lang="ts">
	import '../app.css';
	import favicon from '#lib/assets/favicon.svg';
	import TopBar from '#lib/components/TopBar.svelte';
	import Sidebar from '#lib/components/Sidebar.svelte';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>codegraph · research cockpit</title>
</svelte:head>

<div class="flex h-dvh flex-col">
	<TopBar />
	<div class="flex min-h-0 flex-1">
		<Sidebar />
		<main class="thin-scroll min-h-0 flex-1 overflow-auto bg-neutral-950">
			{@render children()}
		</main>
	</div>
</div>
