import assert from 'node:assert/strict';
import {test} from 'node:test';
import {chromium} from 'playwright';
import {startDev} from './helper.ts';
import {createSdk} from '../../src/graphql/sdk.ts';
import {createViewerClient} from '../../src/graphql/client.ts';

test('responsive panes and live row measurements do not produce a resize delivery loop',{timeout:90000},async t=>{
  const dev=await startDev(),original=globalThis.fetch;let browser;
  const pageErrors:string[]=[];
  try{
    browser=await chromium.launch();
    const page=await browser.newPage({viewport:{width:1440,height:1000}});
    page.on('pageerror',error=>pageErrors.push(error.message));
    // Vite handles window errors before Playwright pageerror; observe the event
    // directly, without preventing it or filtering the development overlay.
    await page.addInitScript(()=>{
      const errors:string[]=[];
      (window as unknown as {resizeTestErrors:string[]}).resizeTestErrors=errors;
      window.addEventListener('error',event=>{if(event.message)errors.push(event.message);});
    });
    await page.goto(dev.origin+'/dev');await page.locator('.shell[aria-hidden="false"]').waitFor();
    const main=page.getByTestId('main-panel');
    await main.getByRole('link',{name:/View thread|Reply in thread/}).first().click();
    const thread=page.getByTestId('thread-panel');await thread.waitFor();
    globalThis.fetch=(url,options)=>original(new URL(url as string,dev.origin),options);
    const sdk=createSdk(createViewerClient()),signal=AbortSignal.timeout(60000),root=page.url().split('/').at(-1)!;
    for(let n=0;n<5;n++)await sdk.Reply({requestId:crypto.randomUUID(),rootId:root,body:`Resize reply ${n} ${'word '.repeat(35)}`},{signal});
    const widths=[1440,1024,768,701,699,390,1200,1440];
    for(let n=0;n<48;n++){
      await page.setViewportSize({width:widths[n%widths.length]!,height:n%2?900:1000});
      if(n%6===0){
        // The main pane is covered by the mobile thread; use the visible pane.
        await thread.getByTestId('message-list').hover();await page.mouse.wheel(0,n%12?-310:450);
        await sdk.Post({requestId:crypto.randomUUID(),channel:'dev',body:`Resize probe ${n} ${'日本語 & '.repeat(20)}`},{signal});
      }
      await page.evaluate(()=>new Promise<void>(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>resolve()))));
    }
    // Observe all committed posts before checking their final layout.
    await page.waitForFunction(()=>document.querySelector('[data-testid="main-panel"] [data-testid="message-list"]')?.getAttribute('data-total')==='58');
    await page.evaluate(()=>new Promise<void>(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>resolve()))));
    assert(await thread.getByTestId('message-list').evaluate(el=>el.scrollHeight>el.clientHeight));
    assert.equal(await main.getByTestId('message-list').getAttribute('data-total'),'58');
    assert.deepEqual(await page.evaluate(()=>(window as unknown as {resizeTestErrors:string[]}).resizeTestErrors),[]);
    assert.deepEqual(pageErrors,[]);assert.equal(await page.locator('vite-error-overlay').count(),0);
    t.diagnostic('48 viewport changes, 5 live replies, 8 live posts; native ResizeObserver, no warning suppression.');
  }catch(error){t.diagnostic(dev.output());throw error;}
  finally{globalThis.fetch=original;await browser?.close();await dev.close();}
});

test('deferred measurements preserve the reading article in every older-page frame',{timeout:90000},async()=>{
  const dev=await startDev();let browser,release=()=>{};
  try{
    browser=await chromium.launch();const page=await browser.newPage({viewport:{width:1440,height:1000}});
    await page.goto(dev.origin+'/dev');await page.locator('.shell[aria-hidden="false"]').waitFor();
    const list=page.getByTestId('main-panel').getByTestId('message-list');
    let entered=()=>{};const started=new Promise<void>(resolve=>entered=resolve),gate=new Promise<void>(resolve=>release=resolve);
    await page.route('**/graphql',async route=>{
      const request=route.request().postDataJSON();
      if(request?.operationName==='Roots' && request.variables.before!==null){entered();await gate;}
      await route.continue().catch(()=>{});
    });
    await list.hover();await page.mouse.wheel(0,-1_000_000);await started;await page.waitForTimeout(300);
    const saved=await list.evaluate(el=>{
      const top=el.getBoundingClientRect().top;
      const row=[...el.querySelectorAll<HTMLElement>('[data-id]')].find(row=>row.querySelector('article')!.getBoundingClientRect().bottom>top+1)!;
      return {id:row.dataset.id!,offset:row.querySelector('article')!.getBoundingClientRect().top-top};
    });
    await list.evaluate((el,saved)=>{
      const state={done:false,samples:[] as (number|null)[]};
      (window as unknown as {anchorTest:typeof state}).anchorTest=state;
      const sample=()=>requestAnimationFrame(()=>{
        const card=el.querySelector(`[data-id="${saved.id}"] article`);
        state.samples.push(card?card.getBoundingClientRect().top-el.getBoundingClientRect().top-saved.offset:null);
        if(!state.done)sample();
      });sample();
    },saved);
    release();
    await page.waitForFunction(()=>Number(document.querySelector('[data-testid="message-list"]')?.getAttribute('data-total'))>50);
    await page.waitForTimeout(200);
    const samples=await page.evaluate(()=>{
      const state=(window as unknown as {anchorTest:{done:boolean;samples:(number|null)[]}}).anchorTest;
      state.done=true;return state.samples;
    });
    assert(samples.length>3);assert(samples.every(delta=>delta!==null&&Math.abs(delta)<4),JSON.stringify(samples));
  }finally{release();await browser?.close();await dev.close();}
});
