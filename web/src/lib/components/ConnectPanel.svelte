<script lang="ts">
	import { workspace } from '#lib/workspace.svelte';

	let path = $state('/Users/hungpham/Desktop/Workspace/codegraph-rs');
	let index = $state(false);
	let busy = $state(false);

	async function connect() {
		busy = true;
		try {
			await workspace.init(path.trim(), index);
		} catch {
			// lỗi đã nằm trong workspace.error
		} finally {
			busy = false;
		}
	}
</script>

<div class="flex h-full items-center justify-center p-6">
	<div class="w-full max-w-lg rounded-lg border border-neutral-800 bg-neutral-900/60 p-6">
		<h2 class="text-lg font-semibold text-neutral-100">Connect a workspace</h2>
		<p class="mt-1 text-sm text-neutral-400">
			Server đã sẵn sàng nhưng chưa bind workspace. Nhập đường dẫn tuyệt đối tới repo.
		</p>
		<div class="mt-4 flex flex-col gap-3">
			<label class="flex flex-col gap-1 text-xs text-neutral-400">
				Workspace root
				<input
					class="rounded border border-neutral-700 bg-neutral-950 px-3 py-2 font-mono text-sm text-neutral-100"
					bind:value={path}
					onkeydown={(e) => e.key === 'Enter' && connect()}
				/>
			</label>
			<label class="flex items-center gap-2 text-sm text-neutral-400">
				<input type="checkbox" bind:checked={index} />
				Index ngay sau khi bind (có thể lâu với repo lớn)
			</label>
			{#if workspace.error}
				<div class="rounded border border-red-900 bg-red-950/40 p-2 text-xs text-red-300">
					{workspace.error}
				</div>
			{/if}
			<button
				class="rounded bg-sky-700 px-4 py-2 text-sm font-medium text-white hover:bg-sky-600 disabled:opacity-50"
				onclick={connect}
				disabled={busy}
			>
				{busy ? 'Connecting…' : 'Connect'}
			</button>
		</div>
	</div>
</div>
