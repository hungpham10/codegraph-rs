import { GraphQLClient } from 'graphql-request';
import { get } from 'svelte/store';
import { settings } from '#lib/settings';

/**
 * Endpoint GraphQL. Mặc định = same-origin (`/graphql`) — đúng khi UI được
 * nhúng trong binary (`codegraph serve --graphql`) và khi chạy dev qua Vite
 * proxy. Có thể override bằng settings (localStorage).
 */
export function graphqlEndpoint(): string {
	const { endpoint } = get(settings);
	if (endpoint) return endpoint.replace(/\/+$/, '');
	if (typeof location !== 'undefined') return `${location.origin}/graphql`;
	return 'http://127.0.0.1:8123/graphql';
}

function client(): GraphQLClient {
	const { apiKey } = get(settings);
	const headers: Record<string, string> = {};
	if (apiKey) headers['Authorization'] = `Bearer ${apiKey}`;
	return new GraphQLClient(graphqlEndpoint(), { headers });
}

/** Chạy một GraphQL query/mutation, trả data đã typed (ném lỗi nếu có). */
export async function gql<T = unknown>(
	document: string,
	variables?: Record<string, unknown>
): Promise<T> {
	return client().request<T>(document, variables);
}

/** Rút message lỗi từ exception graphql-request cho UI. */
export function gqlError(e: unknown): string {
	if (e instanceof Error) return e.message;
	return String(e);
}
