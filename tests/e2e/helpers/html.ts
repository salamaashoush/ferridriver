import type { Page } from '@ferridriver/test';

export function dataUrl(html: string): string {
  return `data:text/html,${encodeURIComponent(html)}`;
}

export async function setBody(page: Page, html: string): Promise<void> {
  await page.setContent(`<!doctype html><html><body>${html}</body></html>`);
}
