import {test,expect} from '@playwright/test';
import {Provider} from './provider.ts';
import {ready,main,list} from './helpers.ts';
let provider:Provider;
test.beforeAll(async()=>{test.setTimeout(360000);provider=new Provider();await provider.start();});
test.afterAll(async()=>{test.setTimeout(120000);await provider?.close();});
test.beforeEach(async()=>{await provider.control({command:'reset'});await provider.control({command:'channel',name:'design'});await provider.control({command:'root',channel:'design',body:'Design fixture'});});
test('cold channel keeps outgoing header/cards until its replacement is complete',async({page})=>{
  await ready(page,provider.url);await provider.control({command:'channel',name:'slow-room'});await provider.control({command:'root',channel:'slow-room',body:'Ready replacement'});
  await main(page).evaluate(el=>(window as any).__chrome={header:el.querySelector('header'),list:el.querySelector('[data-testid="message-list"]')});
  let release!:()=>void,entered!:()=>void;const gate=new Promise<void>(r=>release=r),started=new Promise<void>(r=>entered=r);
  await page.route('**/graphql',async route=>{const q=route.request().postDataJSON();if(q?.operationName==='Roots'&&q.variables.channel==='slow-room'){entered();await gate;}await route.continue().catch(()=>{});});
  await page.getByRole('navigation').getByRole('link',{name:/^slow-room/}).click();await started;
  await expect(main(page).locator('.channel-name')).toHaveText('# dev');await expect(page.locator('main')).toHaveAttribute('aria-busy','true');expect(await main(page).locator('article').count()).toBeGreaterThan(0);
  release();await expect(main(page).locator('.channel-name')).toHaveText('# slow-room');await expect(main(page).getByText('Ready replacement',{exact:true})).toBeVisible();
  expect(await main(page).evaluate(el=>{const saved=(window as any).__chrome;return saved.header===el.querySelector('header')&&saved.list===el.querySelector('[data-testid="message-list"]');})).toBe(true);
});
test('membership failure retains mounted chrome and an explicit Retry restores metadata',async({page})=>{
  await ready(page,provider.url);const header=main(page).locator('header');await header.evaluate(el=>(window as any).__header=el);
  let fail=true;await page.route('**/graphql',async route=>{const q=route.request().postDataJSON();if(fail&&q?.operationName==='Members')await route.fulfill({status:503,body:'unavailable'});else await route.continue();});
  await page.getByRole('navigation').getByRole('link',{name:/^design(?: |$)/}).click();await expect(main(page).locator('.channel-members')).toHaveText('— agents');
  expect(await header.evaluate(el=>el===(window as any).__header)).toBe(true);await expect(page.getByRole('navigation').locator('.membership')).toBeVisible();fail=false;await page.getByRole('navigation').getByRole('button',{name:'Retry'}).click();await expect(main(page).locator('.channel-members')).toHaveText('1 agent');
});
test('one full viewport cover lasts at least one second and waits for slower history',async({page})=>{
  await page.addInitScript(()=>{const sample=()=>requestAnimationFrame(()=>{const cover=document.querySelector('[aria-label="Loading conversations"]');if(cover&&!(window as any).__coverAt)(window as any).__coverAt=performance.now();if((window as any).__coverAt&&!cover){(window as any).__uncoveredAt=performance.now();return;}sample();});sample();});
  let release!:()=>void,entered!:()=>void;const gate=new Promise<void>(r=>release=r),started=new Promise<void>(r=>entered=r);
  await page.route('**/graphql',async route=>{if(route.request().postDataJSON()?.operationName==='Roots'){entered();await gate;}await route.continue().catch(()=>{});});
  await page.goto(provider.url);await started;await page.waitForTimeout(1100);await expect(page.locator('.shell')).toHaveAttribute('inert','');await expect(page.getByRole('status',{name:'Loading conversations'})).toBeVisible();release();
  await expect(page.locator('.shell')).toHaveAttribute('aria-hidden','false');await expect.poll(()=>page.evaluate(()=>(window as any).__uncoveredAt-(window as any).__coverAt)).toBeGreaterThanOrEqual(950);
});
