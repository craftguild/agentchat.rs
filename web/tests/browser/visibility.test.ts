import { test, expect } from '@playwright/test';
import { Provider } from './provider.ts';
import { NativeBrowser } from './native-browser.ts';
import { main, list as messageList } from './helpers.ts';
import { writeFile } from 'node:fs/promises';

test('native background tabs abort the source and foreground tabs replay complete missed changes', async ({}, info) => {
  test.setTimeout(360_000);
  const provider = new Provider(), owned = new NativeBrowser();
  const observations: object[] = [];
  try {
    await provider.start();
    const browser = await owned.start(true);
    const context = browser.contexts()[0]!, page = context.pages()[0]!;
    await page.setViewportSize({width:1440,height:1000});
    const errors: string[] = [], requests: any[] = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('request', request => { if (request.url().endsWith('/graphql')) requests.push(request.postDataJSON()); });
    await page.goto(provider.url);
    await expect(page.locator('.bar .status')).toContainText('Live');
    const pane = main(page), list = messageList(pane);
    await expect(list).toHaveAttribute('data-total','50');
    await list.hover(); await page.mouse.wheel(0,1_000_000);
    await page.evaluate(() => {
      (window as any).__agentchatVisibility = [document.visibilityState];
      document.addEventListener('visibilitychange', () => (window as any).__agentchatVisibility.push(document.visibilityState));
    });
    const before = await provider.control({command:'status'});
    const priorRequests = requests.length;
    const other = await context.newPage(); await other.goto('about:blank'); await other.bringToFront();
    await expect.poll(() => page.evaluate(() => document.visibilityState)).toBe('hidden');
    await expect(page.locator('.bar .status')).toContainText('Paused');
    await expect.poll(async () => { const current = await provider.control({command:'status'}); return current.starts-current.drops; }).toBe(0);
    await provider.control({command:'root',body:'Hidden complete burst',count:65});
    await provider.control({command:'reply',root:'00000000-0000-0000-0000-000000000083',body:'Hidden old-root reply'});
    await provider.control({command:'channel',name:'hidden-review'});
    // Observe absence of the previous one-second polling behavior while the
    // actual native document is hidden; this is no synthetic visibility shim.
    await new Promise(resolve => setTimeout(resolve,1200));
    expect(requests.length).toBe(priorRequests);
    await expect(list).toHaveAttribute('data-total','50');
    const hidden = await provider.control({command:'status'});
    expect(hidden.starts-hidden.drops).toBe(0);
    observations.push({phase:'hidden',before,hidden,requests:requests.length});
    await other.close(); await page.bringToFront();
    await expect.poll(() => page.evaluate(() => document.visibilityState)).toBe('visible');
    await expect(page.locator('.bar .status')).toContainText('Live');
    await expect(list).toHaveAttribute('data-total','115');
    await expect(pane.getByRole('link',{name:/View thread \(76\)/})).toBeVisible();
    await expect(page.getByRole('link',{name:'hidden-review',exact:true}).first()).toBeVisible();
    const resumed = requests.slice(priorRequests);
    const subscriptions=resumed.filter(q => q.query.startsWith('subscription'));
    expect(subscriptions).toHaveLength(1);
    expect(resumed.every(q => q.query.startsWith('subscription') || q.query.includes('channels(') || (q.query.includes('agents(') || q.query.includes('channelMembers(')))).toBe(true);
    expect(subscriptions[0].query.startsWith('subscription')).toBe(true);
    expect(subscriptions[0].variables.after).toEqual({projectId:before.head.project_id,seq:before.head.seq,eventId:before.head.event_id});
    const current = await provider.control({command:'status'});
    expect(current.starts-before.starts).toBe(1); expect(current.starts-current.drops).toBe(1);
    expect(await page.evaluate(() => (window as any).__agentchatVisibility)).toEqual(['visible','hidden','visible']);
    expect(errors).toEqual([]);
    observations.push({phase:'resumed',current,browser:browser.version(),mode:'headless-native-tabs',visibility:['visible','hidden','visible']});
    await page.screenshot({path:info.outputPath('native-visible.png')});
  } finally {
    try { await owned.close(); }
    finally {
      try { await provider.close(); }
      finally {
        const path = info.outputPath('native-visibility-observations.json');
        await writeFile(path,JSON.stringify(observations,null,2));
        await info.attach('native-visibility-observations',{path,contentType:'application/json'});
      }
    }
  }
});
