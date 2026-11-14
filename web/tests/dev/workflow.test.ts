import assert from 'node:assert/strict';
import {test} from 'node:test';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {chromium} from 'playwright';
import {startDev,freePort} from './helper.ts';

test('independent dev serves generated React operations, local assets and live Mock posts',{timeout:90000},async()=>{
  const dev=await startDev();let browser;try{
    browser=await chromium.launch();
    const page=await browser.newPage({viewport:{width:1440,height:1000}}),foreign:string[]=[];page.on('request',r=>{if(!r.url().startsWith(dev.origin))foreign.push(r.url());});
    await page.goto(dev.origin);await page.locator('.shell[aria-hidden="false"]').waitFor();await page.getByTestId('message-list').first().waitFor();
    assert.equal(await page.getByTestId('main-panel').locator('.channel-name').innerText(),'# dev');assert.equal(await page.getByTestId('message-list').first().getAttribute('data-total'),'50');
    assert.equal(await page.getByRole('navigation').locator('.agent').count(),3);assert.equal(await page.locator('.brand').evaluate(el=>getComputedStyle(el).fontSize),'18px');
    assert.match(await page.getByRole('navigation').locator('footer').textContent(),/^v0\.1\.0 \(([a-f0-9]{7}|unknown)\)$/);
    assert.match(await page.locator('.message-content').first().evaluate(el=>getComputedStyle(el).fontFamily),/IBM Plex Sans/);
    assert.equal(await page.locator('article').first().evaluate(el=>getComputedStyle(el).borderRadius),'6px');assert.deepEqual(foreign,[]);
  }finally{await browser?.close();await dev.close();}
});
test('a missing Chigiri executable fails clearly without starting a viewer',{timeout:20000},async()=>{
  const child=spawn(process.execPath,['scripts/dev.mjs'],{env:{...process.env,CHIGIRI_BIN:'/no-such-agentchat-dev-chigiri',AGENTCHAT_DEV_PORT:String(await freePort())},stdio:['ignore','pipe','pipe']});let output='';for(const s of [child.stdout,child.stderr])s.on('data',d=>output+=d);const [code]=await once(child,'close');assert.notEqual(code,0);assert.match(output,/Chigiri|chigiri/);assert.doesNotMatch(output,/Viewer: /);
});
