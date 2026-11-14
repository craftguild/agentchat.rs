import assert from 'node:assert/strict';
import {test} from 'node:test';
import {chromium} from 'playwright';
import {startDev} from './helper.ts';

test('React user forms validate inline, retain scoped drafts and post/reply as user', {timeout:90000},async()=>{
  const dev=await startDev();let browser;
  try{
    browser=await chromium.launch();
    const page=await browser.newPage({viewport:{width:1440,height:1000}}),errors:string[]=[];page.on('pageerror',e=>errors.push(e.message));
    await page.goto(dev.origin+'/random');const main=page.getByTestId('main-panel');await main.getByRole('textbox').waitFor({state:'visible'});await page.locator('.shell[aria-hidden="false"]').waitFor();
    await page.waitForFunction(()=>document.querySelector('.channel-members')?.textContent==='1 agent');
    const input=main.getByRole('textbox');await input.fill('@');assert.equal(await page.getByRole('option').count(),4);await input.press('ArrowDown');await input.press('Enter');assert.equal(await input.inputValue(),'@here ');await input.fill('Unsent random draft');
    await page.getByRole('button',{name:'Create channel',exact:true}).click();const name=page.getByLabel('Channel name',{exact:true});assert.equal(await name.evaluate(el=>el===document.activeElement),true);
    const create=page.getByRole('button',{name:'Create',exact:true});assert.equal(await create.isDisabled(),true);
    for(const [value,valid] of [['a',true],['9dev',false],['dev--tools',false],['dev-',false],['日本語',false],['bad name',false],['a'.repeat(64),true],['a'.repeat(65),false],[' Design-2 ',true]] as const){
      await name.fill(value);await page.waitForFunction(valid=>document.querySelector('#channel-name')?.getAttribute('aria-invalid')===String(!valid),valid);
      assert.equal(await create.isDisabled(),!valid);assert.equal(await page.locator('.field-state svg').count(),1);
    }
    assert.equal(await page.getByText('Uppercase letters become lowercase.',{exact:false}).count(),0);
    await name.fill(' React-User ');await create.click();await page.waitForURL(dev.origin+'/react-user');
    const editor=main.getByRole('textbox',{name:'Message #react-user',exact:true});await editor.fill('  Root from user  ');await editor.press('Enter');await main.getByText('Root from user',{exact:true}).waitFor().catch(async error=>{throw Error(JSON.stringify({error:String(error),url:page.url(),text:await main.innerText(),status:await page.locator('.bar .status').innerText(),pageErrors:errors}));});
    assert.equal(await main.locator('.message-content pre').count(),0);
    const card=main.locator('[data-id]').filter({hasText:'Root from user'});await card.getByRole('link',{name:'Reply in thread',exact:true}).click();
    const thread=page.getByTestId('thread-panel');await thread.getByRole('textbox').fill('  Reply from user  ');await thread.getByRole('textbox').press('Enter');await thread.getByText('Reply from user',{exact:true}).waitFor();
    for(const pane of [main,thread]){
      const aligned=await pane.evaluate(el=>{const card=el.querySelector('article')!.getBoundingClientRect(),editor=el.querySelector('.editor')!.getBoundingClientRect(),footer=el.querySelector('form')!,style=getComputedStyle(footer);return {delta:Math.max(Math.abs(card.left-editor.left),Math.abs(card.right-editor.right)),background:style.backgroundColor,panel:getComputedStyle(el).backgroundColor,border:style.borderTopWidth};});
      assert(aligned.delta<1,JSON.stringify(aligned));assert.equal(aligned.background,aligned.panel);assert.equal(aligned.border,'1px');
    }
    await page.keyboard.press('Escape');await page.getByRole('navigation').getByRole('link',{name:'random',exact:true}).click();const restored=main.getByRole('textbox',{name:'Message #random',exact:true});await restored.waitFor();assert.equal(await restored.inputValue(),'Unsent random draft');assert.deepEqual(errors,[]);
  }finally{await browser?.close();await dev.close();}
});
