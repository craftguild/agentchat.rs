// Native event-result parsing, with no session, sign-in, model or tool execution.
import { expect, mock, test } from 'claude-code/testing';

test('stale owned input blocks before the native core', async ($, on) => {
  mock.env(on, {});
  let reached = false;
  on('classic.UserPromptSubmit', () => { reached = true; return {}; });
  const result = await $.classic.UserPromptSubmit({
    prompt: '<task-notification>AGENTCHAT_INPUT_NOTICE root=stale',
  });
  expect(reached).toBe(false);
  expect(result.block).toBe('AgentChat queued input lost its original native permit');
});

test('ordinary input still reaches the native core', async ($, on) => {
  mock.env(on, {});
  let reached = 0;
  on('classic.UserPromptSubmit', () => { reached++; return {}; });
  const result = await $.classic.UserPromptSubmit({ prompt: 'ordinary user input' });
  expect(reached).toBe(1);
  expect(result.block).toBe(undefined);
});

test('incomplete managed startup blocks the original input', async ($, on) => {
  const root='1a30909c-322a-4585-93e9-c3d941c8cfb2';
  mock.env(on, {AGENTCHAT_LAUNCH_ROOT:root,AGENTCHAT_LAUNCH_GENERATION:'616f2f77-d7f1-498b-b9ec-b35e6c890b88'});
  on('session.id',()=>({value:root}));on('session.cwd',()=>({value:'/test'}));
  on('session.version',()=>({value:{version:'2.1.302'}}));
  on('process.run',()=>({value:{exitCode:0,stdout:'{"ready":false,"cacheable":false}',stderr:''}}));
  let reached=false;on('prompt.submit',()=>{reached=true;return {text:'original'};});
  const result=await $.prompt.submit({text:'original',origin:{kind:'composer'}});
  expect(reached).toBe(false);expect(result.drop).toBe('AgentChat startup is not complete; retry after initialization');
});

test('managed startup helper failure cannot pass the original input', async ($, on) => {
  const root='1a30909c-322a-4585-93e9-c3d941c8cfb2';
  mock.env(on, {AGENTCHAT_LAUNCH_ROOT:root,AGENTCHAT_LAUNCH_GENERATION:'616f2f77-d7f1-498b-b9ec-b35e6c890b88'});
  on('session.id',()=>({value:root}));on('session.cwd',()=>({value:'/test'}));
  on('session.version',()=>({value:{version:'2.1.302'}}));
  on('process.run',()=>({deny:'fixture failure'}));
  let reached=false;on('prompt.submit',()=>{reached=true;return {text:'original'};});
  const result=await $.prompt.submit({text:'original',origin:{kind:'composer'}});
  expect(reached).toBe(false);expect(result.drop).toBe('AgentChat guard unavailable');
});
