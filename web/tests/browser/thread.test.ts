import {test,expect} from '@playwright/test';
import {Provider} from './provider.ts';
import {ready,main,thread,list,cards,top,bottom,anchor,stable} from './helpers.ts';
let provider:Provider;
test.beforeAll(async()=>{test.setTimeout(360000);provider=new Provider();await provider.start();});
test.afterAll(async()=>{test.setTimeout(120000);await provider?.close();});
test.beforeEach(async()=>{await provider.control({command:'reset'});});
test('short live replies retain uniform gaps through scroll and responsive resizing',async({page})=>{
  await ready(page,provider.url);await provider.control({command:'root',body:'Thread spacing parent'});
  await main(page).getByRole('button',{name:/Newest Message/}).click();
  const parent=cards(main(page)).filter({hasText:'Thread spacing parent'}),root=await parent.getAttribute('data-id');await parent.getByRole('link',{name:'Reply in thread',exact:true}).click();
  await expect(thread(page)).toBeVisible();await provider.control({command:'reply',root,body:'A short live reply',count:3});await expect(thread(page).locator('[data-id]:not([data-pinned])[data-full]')).toHaveCount(3);
  for(const width of [1440,600,1440]){
    await page.setViewportSize({width,height:1000});await bottom(thread(page));
    await expect.poll(()=>thread(page).evaluate(el=>{
      const rows=[...el.querySelectorAll<HTMLElement>('[data-id]:not([data-pinned])')];
      return Math.max(...rows.slice(1).map((row,i)=>Math.abs(row.querySelector('article')!.getBoundingClientRect().top-rows[i]!.querySelector('article')!.getBoundingClientRect().bottom-parseFloat(getComputedStyle(rows[i]!).paddingBottom))));
    })).toBeLessThan(1);
    const geometry=await thread(page).evaluate(el=>{const card=el.querySelector('article')!.getBoundingClientRect(),editor=el.querySelector('.editor')!.getBoundingClientRect();return [Math.abs(card.left-editor.left),Math.abs(card.right-editor.right)];});expect(geometry.every(n=>n<1)).toBe(true);
  }
});
test('thread New Messages expires after displaying its latest reply without moving the reading article',async({page})=>{
  const root='00000000-0000-0000-0000-000000000083';await ready(page,provider.url+`dev/${root}`);
  await provider.control({command:'reply',root,body:'Fresh thread comment',count:3});await expect(thread(page).getByRole('heading',{name:'New Messages',exact:true})).toHaveCount(1);
  const replies=thread(page).getByText('Fresh thread comment',{exact:true});await expect(replies).toHaveCount(3);
  await expect(main(page).getByRole('heading',{name:'New Messages',exact:true})).toHaveCount(0);await bottom(thread(page));
  await expect(replies.last()).toBeInViewport();await expect(thread(page).getByRole('button',{name:/Newest Message/})).toHaveCount(0);
  await list(thread(page)).hover();await page.mouse.wheel(0,-100);await page.waitForTimeout(350);const saved=await anchor(thread(page));
  await expect(thread(page).getByRole('heading',{name:'New Messages',exact:true})).toHaveCount(0,{timeout:12000});await stable(thread(page),saved!);
});
test('pinned root recovery has a measured separator with equal space at both widths',async({page})=>{
  await ready(page,provider.url+'dev/00000000-0000-0000-0000-000000000083');
  for(const width of [1440,600]){
    await page.setViewportSize({width,height:1000});await top(thread(page));await top(thread(page));
    const parent=thread(page).locator('[data-thread-separator]');await expect(parent).toBeInViewport();
    const values=await parent.evaluate(el=>{const divider=el.lastElementChild!;return {below:parseFloat(getComputedStyle(el).paddingBottom),above:parseFloat(getComputedStyle(divider).marginTop),border:parseFloat(getComputedStyle(divider).borderTopWidth)};});
    expect(values).toEqual({below:width<=700?12:16,above:width<=700?12:16,border:1});
  }
});
