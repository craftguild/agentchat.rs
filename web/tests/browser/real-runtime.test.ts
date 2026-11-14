import {test,expect} from '@playwright/test';
import {Provider} from './provider.ts';
import {NativeBrowser} from './native-browser.ts';
import {ready,main,thread,list,top,bottom} from './helpers.ts';

test('React observes real SQLite history, large safe bodies and a durable runtime restart without read/ACK side effects',async({page},info)=>{
  test.setTimeout(360000);const provider=new Provider('real'),errors:string[]=[];page.on('pageerror',e=>errors.push(e.message));
  try{
    await provider.start();const initial=await provider.control({command:'status'});await ready(page,provider.url);
    await expect(list(main(page))).toHaveAttribute('data-total','50');await top(main(page));await expect(list(main(page))).toHaveAttribute('data-total','100');await bottom(main(page));
    await main(page).getByRole('link',{name:/View thread \(75\)/}).click();await expect(thread(page)).toBeVisible();await top(thread(page));await top(thread(page));await expect(thread(page).locator('[data-pinned]')).toHaveAttribute('data-id',initial.thread_root);await expect(list(thread(page))).toHaveAttribute('data-total','76');
    await thread(page).getByRole('button',{name:'Close thread',exact:true}).click();
    let total=Number(await list(main(page)).getAttribute('data-total'));
    for(const [char,n] of [['💜',16384],['&',65536]] as const){
      await provider.control({command:'root',body:char.repeat(n)});await expect(list(main(page))).toHaveAttribute('data-total',String(++total));await bottom(main(page));
      await expect.poll(()=>main(page).locator('.message-content').evaluateAll((els,expected)=>els.some(el=>el.textContent===expected),char.repeat(n))).toBe(true);
    }
    await provider.control({command:'root',body:'<script>window.__xss=1</script>\n\n[bad](javascript:alert(1))\n\n|A|B|\n|---|---|\n|x|y|'});await expect(list(main(page))).toHaveAttribute('data-total',String(++total));await bottom(main(page));await expect(main(page).locator('.message-content table')).toBeVisible();expect(await page.evaluate(()=>(window as any).__xss)).toBeUndefined();
    const before=await provider.control({command:'status'}),restarted=await provider.control({command:'restart'});expect(restarted.runtime_instance).not.toBe(before.runtime_instance);expect(restarted.head).toEqual(before.head);
    await provider.control({command:'root',body:'After durable restart'});await expect(page.locator('.bar .status')).toContainText('Live');
    const received=main(page).getByText('After durable restart',{exact:true});await expect(received).toBeAttached();await bottom(main(page));await expect(received).toBeInViewport();
    const current=await provider.control({command:'status'});expect(current.statistics.read_receipts).toBe(0);expect(current.statistics).not.toHaveProperty('acknowledged_deliveries');expect(current.author_id).toBe(initial.author_id);expect(errors).toEqual([]);
    await info.attach('real-react-observer',{body:JSON.stringify({initial,current}),contentType:'application/json'});
    const stopped=await provider.control({command:'stop_viewer'});expect(stopped.observers).toBe(0);expect(stopped.statistics).toEqual(current.statistics);
  }finally{await provider.close();}
});

test('native hidden tabs release the real RPC source and replay committed SQLite posts',async()=>{
  test.setTimeout(360000);const provider=new Provider('real'),owned=new NativeBrowser();
  try{
    await provider.start();const browser=await owned.start(true),context=browser.contexts()[0]!,page=context.pages()[0]!;
    await ready(page,provider.url);await expect.poll(async()=>(await provider.control({command:'status'})).observers).toBe(2);
    const requests:any[]=[];page.on('request',r=>{if(r.url().endsWith('/graphql'))requests.push(r.postDataJSON());});
    const other=await context.newPage();await other.bringToFront();await expect.poll(()=>page.evaluate(()=>document.visibilityState)).toBe('hidden');
    await expect.poll(async()=>(await provider.control({command:'status'})).observers).toBe(1);const count=requests.length;
    await provider.control({command:'root',body:'Hidden real SQLite',count:65});await page.waitForTimeout(1200);expect(requests).toHaveLength(count);await expect(list(main(page))).toHaveAttribute('data-total','50');
    await other.close();await page.bringToFront();await expect.poll(()=>page.evaluate(()=>document.visibilityState)).toBe('visible');await expect(list(main(page))).toHaveAttribute('data-total','115');
    expect(requests.filter(q=>q.operationName==='Live')).toHaveLength(1);await expect.poll(async()=>(await provider.control({command:'status'})).observers).toBe(2);
    const stopped=await provider.control({command:'stop_viewer'});expect(stopped.observers).toBe(0);
  }finally{await owned.close();await provider.close();}
});
