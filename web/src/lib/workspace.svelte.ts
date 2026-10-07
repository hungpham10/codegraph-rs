import { QS, MS } from '#lib/graphql/queries';
import { gql, gqlError } from '#lib/graphql/client';
import type { Status } from '#lib/graphql/queries';

/**
 * Trạng thái workspace toàn cục (runes trong `.svelte.ts`).
 * UI hỏi `status` để biết đã có index chưa; nếu chưa → panel Connect.
 */
class Workspace {
	status = $state<Status | null>(null);
	connected = $state(false);
	loading = $state(false);
	error = $state<string | null>(null);
	/** Root đã bind (suy ra từ lần connect gần nhất). */
	root = $state<string | null>(null);
	/** Module UI đang bật (từ config server). */
	uiModules = $state<string[]>(['explore', 'review', 'documents']);

	/** Module này bật không? */
	hasModule(id: string): boolean {
		return this.uiModules.includes(id);
	}

	async loadUiConfig() {
		try {
			const data = await gql<{ uiConfig: string[] }>(QS.uiConfig);
			if (data.uiConfig?.length) this.uiModules = data.uiConfig;
		} catch {
			// giữ mặc định
		}
	}

	async refresh() {
		this.loading = true;
		this.error = null;
		try {
			const data = await gql<{ status: Status }>(QS.status);
			this.status = data.status;
			this.connected = true;
		} catch (e) {
			this.connected = false;
			this.status = null;
			this.error = gqlError(e);
		} finally {
			this.loading = false;
		}
	}

	async init(path: string, index: boolean) {
		this.loading = true;
		this.error = null;
		try {
			await gql(MS.init, { path, index });
			this.root = path;
			await this.refresh();
		} catch (e) {
			this.connected = false;
			this.error = gqlError(e);
			throw e;
		} finally {
			this.loading = false;
		}
	}
}

export const workspace = new Workspace();
