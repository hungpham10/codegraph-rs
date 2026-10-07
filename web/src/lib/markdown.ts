import { marked } from 'marked';

marked.setOptions({ gfm: true, breaks: false });

/** Render markdown (context output) thành HTML. */
export function renderMarkdown(md: string): string {
	return marked.parse(md ?? '') as string;
}
