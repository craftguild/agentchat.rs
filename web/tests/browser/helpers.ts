import { expect } from '@playwright/test';
import type { Page,Locator } from '@playwright/test';
export const main=(page:Page)=>page.getByTestId('main-panel');
export const thread=(page:Page)=>page.getByTestId('thread-panel');
export const list=(pane:Locator)=>pane.getByTestId('message-list');
export const cards=(pane:Locator)=>pane.locator('[data-id]');
export async function ready(page:Page,url:string){
  await page.goto(url);await expect(page.locator('.bar .status')).toContainText('Live');
  await expect(page.locator('.shell')).toHaveAttribute('aria-hidden','false');
  await expect(list(main(page))).toBeVisible();
}
export async function bottom(pane:Locator){
  await list(pane).hover();await pane.page().mouse.wheel(0,1_000_000);
  await expect.poll(()=>list(pane).evaluate(el=>Math.abs(el.scrollHeight-el.clientHeight-el.scrollTop))).toBeLessThan(3);
  await pane.page().waitForTimeout(300);
}
export async function top(pane:Locator){
  await list(pane).hover();await pane.page().mouse.wheel(0,-1_000_000);await pane.page().waitForTimeout(350);
}
export async function anchor(pane:Locator){
  return list(pane).evaluate(el=>{
    const top=el.getBoundingClientRect().top;
    const row=[...el.querySelectorAll<HTMLElement>('[data-id]')].find(row=>row.querySelector('article')!.getBoundingClientRect().bottom>top+1);
    return row?{id:row.dataset.id!,offset:row.querySelector('article')!.getBoundingClientRect().top-top}:null;
  });
}
export async function stable(pane:Locator,saved:{id:string;offset:number}){
  await expect.poll(()=>list(pane).evaluate((el,saved)=>{
    const article=el.querySelector(`[data-id="${saved.id}"] article`);
    return article?Math.abs(article.getBoundingClientRect().top-el.getBoundingClientRect().top-saved.offset):Infinity;
  },saved)).toBeLessThan(4);
}
