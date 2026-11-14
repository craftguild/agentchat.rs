import {test,expect} from '@playwright/test';
import {Provider} from './provider.ts';
import {ready,main,thread,list,top,anchor,stable} from './helpers.ts';
let provider:Provider;
test.beforeAll(async()=>{test.setTimeout(360000);provider=new Provider();await provider.start();});
test.afterAll(async()=>{test.setTimeout(120000);await provider?.close();});
test.beforeEach(async()=>{await provider.control({command:'reset'});await provider.control({command:'channel',name:'design'});await provider.control({command:'root',channel:'design',body:'Design fixture'});});
test('short URLs support direct thread entry, browser history, Escape and modified native links',async({page})=>{
  const id='00000000-0000-0000-0000-000000000083';await ready(page,provider.url+`dev/${id}`);
  await expect(thread(page)).toBeVisible();await top(thread(page));await top(thread(page));await expect(thread(page).locator('[data-pinned]')).toHaveAttribute('data-id',id);await page.keyboard.press('Escape');await expect(page).toHaveURL(provider.url+'dev');
  await main(page).getByRole('link',{name:/View thread \(75\)/}).click();await expect(thread(page)).toBeVisible();await page.goBack();await expect(thread(page)).toHaveCount(0);await page.goForward();await expect(thread(page)).toBeVisible();
  await thread(page).getByRole('button',{name:'Close thread',exact:true}).click();
  const design=page.getByRole('navigation').getByRole('link',{name:/^design(?: |$)/});await expect(design).toHaveAttribute('href','/design');
  const opened=page.context().waitForEvent('page');await design.click({modifiers:['ControlOrMeta']});const popup=await opened;await popup.waitForURL(provider.url+'design');await popup.close();expect(page.url()).toBe(provider.url+'dev');
});
test('220px sidebar becomes a keyboard-contained mobile overlay and preserves main geometry',async({page})=>{
  await ready(page,provider.url);const nav=page.getByRole('navigation'),menu=page.locator('.menu-button');
  for(const width of [1440,1024,768]){await page.setViewportSize({width,height:900});await expect(nav).toHaveCSS('width','220px');await expect(menu).toBeHidden();}
  await page.setViewportSize({width:390,height:844});await expect(nav).toBeHidden();const left=await main(page).evaluate(el=>el.getBoundingClientRect().left);
  await expect(page.locator('#project-navigation')).toHaveCSS('display','none');
  await page.evaluate(()=>new Promise<void>(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>resolve()))));
  await menu.click();await expect(nav).toBeVisible();await expect(menu).toHaveAttribute('aria-expanded','true');await expect(page.locator('main')).toHaveAttribute('inert','');
  await expect(nav.getByRole('link',{name:/^dev(?: |$)/})).toBeFocused();await menu.focus();await page.keyboard.press('Shift+Tab');expect(await nav.evaluate(el=>el.contains(document.activeElement))).toBe(true);
  await page.keyboard.press('Escape');await expect(nav).toBeHidden();await expect(menu).toBeFocused();expect(await main(page).evaluate(el=>el.getBoundingClientRect().left)).toBe(left);
  await menu.click();await nav.getByRole('link',{name:/^design(?: |$)/}).click();await expect(nav).toBeHidden();await expect(main(page).locator('.channel-name')).toHaveText('# design');
});
test('the Active plus button aligns with the selected channel right edge',async({page})=>{
  await ready(page,provider.url);const nav=page.getByRole('navigation');
  const plus=await nav.getByRole('button',{name:'Create channel',exact:true}).boundingBox(),active=await nav.locator('[aria-current="page"]').boundingBox();
  expect(Math.abs(plus!.x+plus!.width-active!.x-active!.width)).toBeLessThan(1);
  expect(await nav.getByRole('button',{name:'Create channel',exact:true}).evaluate(el=>getComputedStyle(el).width)).toBe('28px');
});

test('older paging preserves the same article in every painted frame',async({page})=>{
  await ready(page,provider.url);let release!:()=>void,entered!:()=>void;const gate=new Promise<void>(r=>release=r),started=new Promise<void>(r=>entered=r);
  await page.route('**/graphql',async route=>{const q=route.request().postDataJSON();if(q?.operationName==='Roots'&&q.variables.before!==null){entered();await gate;}await route.continue().catch(()=>{});});
  await list(main(page)).hover();await page.mouse.wheel(0,-1_000_000);await started;await page.waitForTimeout(300);const saved=await anchor(main(page));expect(saved).not.toBeNull();
  await list(main(page)).evaluate((el,saved)=>{
    (window as any).__anchorDone=false;(window as any).__anchorFrames=[];
    const sample=()=>requestAnimationFrame(()=>{
      const card=el.querySelector(`[data-id="${saved!.id}"] article`);(window as any).__anchorFrames.push(card?card.getBoundingClientRect().top-el.getBoundingClientRect().top-saved!.offset:null);
      if(!(window as any).__anchorDone)sample();
    });sample();
  },saved);
  release();await expect(list(main(page))).toHaveAttribute('data-total','100');await stable(main(page),saved!);
  await page.waitForTimeout(200);const samples=await page.evaluate(()=>{(window as any).__anchorDone=true;return (window as any).__anchorFrames;});
  expect(samples.length).toBeGreaterThan(3);expect(samples.every((delta:number|null)=>delta!==null&&Math.abs(delta)<4)).toBe(true);
});
