import { test, expect } from '@playwright/test';
import { Provider } from './provider.ts';

test('project roster retains every participant while real channel membership changes its emphasis',async ({page},info) => {
  test.setTimeout(360_000); const provider=new Provider('real');
  try {
    await provider.start(); await provider.control({command:'participants',count:2});
    await provider.control({command:'channel',name:'scoped-room'}); await page.goto(provider.url);
    const sidebar=page.getByRole('navigation'), rows=sidebar.locator('.agent'), muted=rows.locator('.muted');
    await expect(sidebar.locator('.member-count')).toHaveCount(0); await expect(rows).toHaveCount(3);
    await expect(sidebar.locator('.membership')).toHaveCount(0); await expect(muted).toHaveCount(0);
    await sidebar.evaluate((el:any) => { (window as any).__navigationNodes={list:el.querySelector('[data-testid="navigation-list"]'),first:el.querySelector('.agent')}; });
    await sidebar.getByRole('link',{name:'scoped-room',exact:true}).click();
    await expect(muted).toHaveCount(3); await expect(page.locator('.root-pane .channel-members')).toHaveText('0 agents');
    await provider.control({command:'membership',channel:'scoped-room',joined:true}); await expect(muted).toHaveCount(2); await expect(page.locator('.root-pane .channel-members')).toHaveText('1 agent');
    await provider.control({command:'membership',index:0,channel:'scoped-room',joined:true}); await expect(muted).toHaveCount(1); await expect(page.locator('.root-pane .channel-members')).toHaveText('2 agents');

    await page.screenshot({path:info.outputPath('channel-membership.png')});
    await provider.control({command:'membership',index:0,channel:'scoped-room',joined:false}); await expect(muted).toHaveCount(2); await expect(page.locator('.root-pane .channel-members')).toHaveText('1 agent');
    await sidebar.getByRole('link',{name:'dev',exact:true}).click(); await expect(muted).toHaveCount(0); await expect(page.locator('.root-pane .channel-members')).toHaveText('3 agents');
    await expect(rows).toHaveCount(3);
    let fail=true;
    await page.route('**/graphql',async route => {
      if (fail && route.request().postDataJSON()?.variables?.channel==='scoped-room') await route.fulfill({status:503,body:'unavailable'});
      else await route.continue();
    });
    await sidebar.getByRole('link',{name:'scoped-room',exact:true}).click();
    await expect(sidebar.locator('.membership [role="alert"]')).toContainText('Membership unavailable'); await expect(page.locator('.root-pane .channel-members')).toHaveText('— agents'); await expect(muted).toHaveCount(0);
    fail=false; await sidebar.locator('.membership').getByRole('button',{name:'Retry'}).click(); await expect(muted).toHaveCount(2); await expect(page.locator('.root-pane .channel-members')).toHaveText('1 agent');
    expect(await sidebar.evaluate((el:any) => (window as any).__navigationNodes.list === el.querySelector('[data-testid="navigation-list"]')
      && (window as any).__navigationNodes.first === el.querySelector('.agent'))).toBe(true);
    const status=await provider.control({command:'status'});
    expect(status.statistics.joined).toBe(3); expect(status.statistics.read_receipts).toBe(0); expect(status.statistics).not.toHaveProperty('acknowledged_deliveries');
    await info.attach('observer-membership-status',{body:JSON.stringify(status),contentType:'application/json'});
  } finally { await provider.close(); }
});
