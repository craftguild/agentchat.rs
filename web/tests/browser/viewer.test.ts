import { test,expect } from '@playwright/test';
import { Provider } from './provider.ts';
import { ready,main,thread,list,cards,top,bottom,anchor,stable } from './helpers.ts';
let provider:Provider,errors:string[]=[];
test.beforeAll(async()=>{test.setTimeout(360000);provider=new Provider();await provider.start();});
test.afterAll(async()=>{test.setTimeout(120000);await provider?.close();});
test.beforeEach(async({page})=>{errors=[];page.on('pageerror',e=>errors.push(e.message));await provider.control({command:'reset'});await provider.control({command:'channel',name:'design'});await provider.control({command:'root',channel:'design',body:'Design fixture'});await ready(page,provider.url);});
test.afterEach(()=>expect(errors).toEqual([]));

test('snapshot pages keep 50 latest roots, older paging preserves the visible article',async({page})=>{
  const pane=main(page);await expect(list(pane)).toHaveAttribute('data-total','50');
  const saved=await anchor(pane);expect(saved).not.toBeNull();
  await top(pane);await expect(list(pane)).toHaveAttribute('data-total','100');
  // The upper boundary retains the article reached by the explicit wheel gesture.
  const first=await anchor(pane);expect(first).not.toBeNull();await stable(pane,first!);
  await top(pane);await expect(list(pane)).toHaveAttribute('data-total','120');
  await bottom(pane);await expect(pane.getByRole('link',{name:/View thread \(75\)/})).toBeVisible();
});
test('live append keeps a reading anchor and Newest Message navigates to the latest received root',async({page})=>{
  const pane=main(page);await list(pane).hover();await page.mouse.wheel(0,-300);await page.waitForTimeout(350);
  const saved=await anchor(pane);expect(saved).not.toBeNull();
  await provider.control({command:'root',body:'A live root without following'});
  await expect(list(pane)).toHaveAttribute('data-total','51');await stable(pane,saved!);
  await pane.getByRole('button',{name:'Newest Message',exact:false}).click();
  await expect(pane.locator('.message-content').filter({hasText:'A live root without following'})).toBeInViewport();
});
test('channel unread counts retain unseen reply holes and reset on a new browser document',async({page})=>{
  const nav=page.locator('nav');await provider.control({command:'channel',name:'unread-room'});
  await provider.control({command:'root',channel:'unread-room',body:'Unread parent'});
  const room=nav.getByRole('link',{name:/^unread-room/,includeHidden:true});await expect(room.locator('.unread')).toHaveText('1');
  await room.click();const parent=main(page).locator('[data-id]').filter({hasText:'Unread parent'});await expect(parent).toBeVisible();await expect(room.locator('.unread')).toHaveCount(0);
  const root=await parent.getAttribute('data-id');await nav.getByRole('link',{name:/^dev(?: |$)/}).click();
  await provider.control({command:'reply',root,body:'Unread reply',count:65});await expect(room.locator('.unread')).toHaveText('65');await room.click();
  await expect(parent.locator('.unread-dot')).toBeVisible();await parent.getByRole('link',{name:/View thread/}).click();
  await expect(thread(page)).toBeVisible();await expect.poll(async()=>Number(await room.locator('.unread').textContent())).toBeLessThan(65);
  expect(Number(await room.locator('.unread').textContent())).toBeGreaterThan(0);
  await thread(page).getByRole('button',{name:'Close thread',exact:true}).click();await expect(parent.locator('.unread-dot')).toBeVisible();
  await page.reload();await expect(page.locator('.bar .status')).toContainText('Live');await expect(room.locator('.unread')).toHaveCount(0);
});
test('root and reply counts update without rebuilding the mounted timeline',async({page})=>{
  const pane=main(page);await list(pane).evaluate(el=>(window as any).__retainedList=el);
  const root='00000000-0000-0000-0000-000000000083';
  await provider.control({command:'reply',root,body:'Count update'});
  await expect(pane.getByRole('link',{name:/View thread \(76\)/})).toBeVisible();
  expect(await list(pane).evaluate(el=>el===(window as any).__retainedList)).toBe(true);
  await expect(list(pane)).toHaveAttribute('data-total','50');
});
test('one SSE source and no history polling survive channel/thread navigation',async({page})=>{
  const queries:any[]=[];page.on('request',r=>{if(r.url().endsWith('/graphql'))queries.push(r.postDataJSON());});
  await main(page).getByRole('link',{name:/View thread \(75\)/}).click();await expect(thread(page)).toBeVisible();
  await thread(page).getByRole('button',{name:'Close thread',exact:true}).click();
  await page.getByRole('navigation').getByRole('link',{name:/^design(?: |$)/}).click();await expect(main(page).locator('.channel-name')).toHaveText('# design');
  await page.waitForTimeout(300);const length=queries.length;await page.waitForTimeout(1300);expect(queries.length).toBe(length);
  expect(queries.filter(q=>q.operationName==='Live')).toHaveLength(0);
});
test('long Markdown grows cards, keeps safe links and does not create nested vertical scrolling',async({page})=>{
  const body='## Long body\n\n'+('日本語 long paragraph **bold**.\n\n'.repeat(70))+'[external](https://example.com)\n\n<script>alert(1)</script>';
  await provider.control({command:'root',body});await main(page).getByRole('button',{name:/Newest Message/}).click();
  const article=main(page).locator('article').filter({hasText:'Long body'});await expect(article).toBeInViewport();
  expect(await article.evaluate(el=>({height:el.clientHeight,overflow:getComputedStyle(el).overflowY}))).toMatchObject({overflow:'visible'});
  expect(await article.evaluate(el=>el.clientHeight)).toBeGreaterThan(1000);await expect(article.locator('script')).toHaveCount(0);
  const link=article.getByRole('link',{name:'external'});await link.click({trial:true});await expect(link).toHaveAttribute('href','https://example.com');
  expect(await page.evaluate(()=>document.querySelectorAll('agentchat-app').length)).toBe(0);
});

test('reported drops, partial EOF and complete events reconnect from the unapplied checkpoint',async({page})=>{
  for(const body of [
    'event: next\ndata: {"data":{"changes":null},"extensions":{"chigiri":{"delivery":{"droppedTotal":0}}}}\n\n',
    'event: next\ndata: {"data":',
    'event: complete\n\n',
  ]){
    const requests:any[]=[];await page.route('**/graphql',async route=>{
      const query=route.request().postDataJSON();if(query?.operationName==='Live'){requests.push(query);if(requests.length===1){await route.fulfill({contentType:'text/event-stream',body});return;}}await route.continue();
    });
    await page.reload();await expect.poll(()=>requests.length).toBe(2);expect(requests[1].variables.after).toEqual(requests[0].variables.after);
    await expect(page.locator('.bar .status')).toContainText('Live');await expect(list(main(page))).toHaveAttribute('data-total','50');await page.unroute('**/graphql');
  }
});
