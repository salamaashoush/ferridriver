// An HTML5 drag works on every load of a page, not only the first: a
// calendar moves an event, reloads, and moves it again.

import { describe, expect, test } from '@ferridriver/test';

const PAGE = `<style>body{margin:0} td{width:150px;height:150px;border:1px solid #ccc;vertical-align:top}</style>
<table><tr>
  <td id="a"><a id="chip" href="#e" draggable="true">Planning</a></td>
  <td id="b"></td><td id="c"></td>
</tr></table>
<script>
  window.drops = [];
  document.getElementById('chip').addEventListener('dragstart', (e) => {
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', 'event-1');
  });
  for (const cell of document.querySelectorAll('td')) {
    cell.addEventListener('dragover', (e) => { e.preventDefault(); e.dataTransfer.dropEffect = 'move'; });
    cell.addEventListener('drop', (e) => { e.preventDefault(); window.drops.push(cell.id + ':' + e.dataTransfer.getData('text/plain')); });
  }
</script>`;

describe('drag and drop across reloads', () => {
  test('a link drags onto a cell, and again after the page reloads', async ({ page, browserName }) => {
    test.skip(browserName === 'firefox', 'BiDi never delivers the drop, as in Playwright');
    await page.route('**/fx/calendar-week', (route) => route.fulfill({ contentType: 'text/html', body: PAGE }));
    await page.goto('/fx/calendar-week');
    await page.locator('#chip').dragTo(page.locator('#b'));
    expect(await page.evaluate(() => (window as any).drops)).toEqual(['b:event-1']);
    await page.reload();
    await page.locator('#chip').dragTo(page.locator('#c'));
    expect(await page.evaluate(() => (window as any).drops)).toEqual(['c:event-1']);
    await page.evaluate(() => document.getElementById('c')!.append(document.getElementById('chip')!));
    await page.locator('#chip').dragTo(page.locator('#a'));
    expect(await page.evaluate(() => (window as any).drops)).toEqual(['c:event-1', 'a:event-1']);
  });
});
