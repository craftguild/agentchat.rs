import {test,expect} from '@playwright/test';
import {Provider} from './provider.ts';
import {ready,main,thread} from './helpers.ts';
let provider:Provider;
test.beforeAll(async()=>{test.setTimeout(360000);provider=new Provider();await provider.start();});
test.afterAll(async()=>{test.setTimeout(120000);await provider?.close();});
test('route and thread errors provide a visible Unlink indicator and retry',async({page})=>{
  await page.goto(provider.url+'missing-channel');await expect(page.getByRole('alert')).toContainText('Channel not found');await expect(page.getByRole('alert').locator('svg')).toHaveCount(1);await expect(page.getByRole('button',{name:'Retry'})).toBeVisible();
  await ready(page,provider.url+'dev/00000000-0000-0000-0000-000000000099');await expect(thread(page).getByRole('alert')).toBeVisible();
});
test('thread and sidebar motion honor desktop, mobile and reduced-motion settings',async({page})=>{
  await ready(page,provider.url);await main(page).getByRole('link',{name:/View thread \(75\)/}).click();
  const drawer=page.locator('.thread-panel');await expect(drawer).toBeVisible();await expect(drawer).toHaveCSS('transition-timing-function',/ease-out/);
  await thread(page).getByRole('button',{name:'Close thread',exact:true}).click();await expect(drawer).toBeHidden();await expect(drawer).toHaveCSS('transition-timing-function',/ease-in/);
  await page.setViewportSize({width:600,height:900});await page.getByRole('button',{name:'Open navigation',exact:true}).click();await expect(page.locator('.navigation-backdrop')).toBeVisible();await page.keyboard.press('Escape');
  await main(page).getByRole('link',{name:/View thread \(75\)/}).click();await expect(drawer).toBeVisible();await expect(drawer).toHaveCSS('transition-duration','0s');await page.keyboard.press('Escape');await expect(drawer).toBeHidden();
  await page.emulateMedia({reducedMotion:'reduce'});await page.setViewportSize({width:1200,height:900});await main(page).getByRole('link',{name:/View thread \(75\)/}).click();await expect(drawer).toHaveCSS('transition-duration','0s');
});
