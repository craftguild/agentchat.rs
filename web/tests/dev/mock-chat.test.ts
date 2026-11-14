import assert from 'node:assert/strict';
import {test} from 'node:test';
import {chromium} from 'playwright';
import {startDev} from './helper.ts';
import {createSdk} from '../../src/graphql/sdk.ts';
import {createViewerClient} from '../../src/graphql/client.ts';

test('Mock mentions echo under the addressed agent names and reconnect catches posts missed during setup',{timeout:90000},async()=>{
  const dev=await startDev(),original=globalThis.fetch;let browser;
  try{
    browser=await chromium.launch();
    globalThis.fetch=(url,options)=>original(new URL(url as string,dev.origin),options);
    const sdk=createSdk(createViewerClient()),signal=AbortSignal.timeout(60000),page=await browser.newPage();let connections=0;
    await page.route('**/graphql',async route=>{
      if(route.request().postDataJSON().operationName==='Live'&&++connections===1){await route.fulfill({contentType:'text/event-stream',body:'event: next\ndata: {"errors":[{"message":"temporary","extensions":{"code":"MOCK_RESOURCE_LIMIT"}}]}\n\n'});return;}await route.continue();
    });
    await page.goto(dev.origin+'/design');
    const body='@blue-otter connection recovery',root=(await sdk.Post({requestId:crypto.randomUUID(),channel:'design',body},{signal})).result.id;
    await page.getByTestId('main-panel').getByText(body,{exact:true}).waitFor();await page.locator('.bar .status').getByText('Live',{exact:true}).waitFor();assert(connections>=2);
    await page.locator('.shell[aria-hidden="false"]').waitFor();const main=page.getByTestId('main-panel'),mention=main.locator('.mention[data-mention="blue-otter"]');await mention.focus();await page.getByRole('tooltip').getByText('blue-otter',{exact:true}).waitFor();await page.keyboard.press('Escape');
    await main.locator(`[data-id="${root}"]`).getByRole('link',{name:/View thread|Reply in thread/}).first().click();const thread=page.getByTestId('thread-panel');await thread.getByText('Echo: '+body,{exact:true}).waitFor();
    const whitespace='\t\n\v\f\r \u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000';
    const stripped=(await sdk.Reply({requestId:crypto.randomUUID(),rootId:root,body:whitespace+'Stored text'+whitespace},{signal})).result.id;
    assert.equal((await sdk.Replies({channel:'design',rootId:root,before:null,snapshot:null,limit:50},{signal})).replies.items.find(m=>m.id===stripped)?.body,'Stored text');
    for(const group of ['all','here','channel']){
      await sdk.Reply({requestId:crypto.randomUUID(),rootId:root,body:`@${group} group echo`},{signal});
      let replies:any[]=[];for(let n=0;n<100;n++){replies=(await sdk.Replies({channel:'design',rootId:root,before:null,snapshot:null,limit:50},{signal})).replies.items.filter(m=>m.body===`Echo: @${group} group echo`);if(replies.length===2)break;await new Promise(r=>setTimeout(r,30));}
      assert.deepEqual(replies.map(m=>m.author.handle).sort(),['blue-otter','violet-fox']);
    }
    for(let n=0;n<8;n++)await sdk.Reply({requestId:crypto.randomUUID(),rootId:root,body:`@blue-otter load ${n} ${'&'.repeat(100)}`},{signal});
    assert.equal(await page.getByText('Reload snapshot',{exact:true}).count(),0);
  }finally{globalThis.fetch=original;await browser?.close();await dev.close();}
});
