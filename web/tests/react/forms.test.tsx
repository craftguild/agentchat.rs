// @vitest-environment jsdom
import { test,expect,vi,afterEach } from 'vitest';
import { render,screen,fireEvent,cleanup,act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Composer } from '../../src/forms/composer.tsx';
import { DraftProvider,draftReducer } from '../../src/forms/drafts.tsx';
import { channelSchema,messageSchema } from '../../src/forms/schemas.ts';
const mocks=vi.hoisted(()=>({send:vi.fn(),reveal:vi.fn()}));
vi.mock('../../src/graphql/provider.tsx',()=>{
  const sdk={
    Post:(v,o)=>mocks.send(v.requestId,v.channel,null,v.body,o.signal).then(result=>({result})),
    Reply:(v,o)=>mocks.send(v.requestId,'dev',v.rootId,v.body,o.signal).then(result=>({result})),
  };return {useSdk:()=>({sdk})};
});
vi.mock('../../src/state/session.tsx',()=>({useSession:()=>({actions:{reveal:mocks.reveal}})}));
afterEach(()=>{cleanup();vi.clearAllMocks();});
const composer=(channel='dev',root:string|null=null)=><DraftProvider><Composer channel={channel} root={root} ready handles={['blue-otter','green-wren']}/></DraftProvider>;
test('channel policy normalizes at the boundary and validates all advertised restrictions',()=>{
  expect(channelSchema.parse({name:' Design-2 '})).toEqual({name:'design-2'});
  for(const name of ['','9dev','dev--tools','dev-','日本語','bad name','a'.repeat(65)])expect(channelSchema.safeParse({name}).success).toBe(false);
  for(const name of ['a','a'.repeat(64),'design-2'])expect(channelSchema.safeParse({name}).success).toBe(true);
  expect(messageSchema.safeParse({body:'\n \t'}).success).toBe(false);
  expect(messageSchema.safeParse({body:'💜'.repeat(16385)}).success).toBe(false);
});
test('RHF mention completion replaces the active token, supports keys and preserves Shift+Enter',async()=>{
  render(composer());const user=userEvent.setup(),input=screen.getByRole('textbox');
  await user.type(input,'@');expect(screen.getAllByRole('option')).toHaveLength(5);
  await user.keyboard('{ArrowDown}{Enter}');expect((input as HTMLTextAreaElement).value).toBe('@here ');expect(mocks.send).not.toHaveBeenCalled();
  await user.clear(input);await user.type(input,'@bl');await user.keyboard('{Tab}');expect((input as HTMLTextAreaElement).value).toBe('@blue-otter ');
  await user.keyboard('{Shift>}{Enter}{/Shift}');expect((input as HTMLTextAreaElement).value).toContain('\n');
});
test('IME Enter never posts; explicit retry keeps the same request ID and raw draft',async()=>{
  mocks.send.mockRejectedValueOnce(Error('temporary')).mockResolvedValue({id:'own-post',warnings:[]});
  render(composer());const user=userEvent.setup(),input=screen.getByRole('textbox');
  await user.type(input,'  日本語  ');fireEvent.compositionStart(input);fireEvent.keyDown(input,{key:'Enter',keyCode:229,isComposing:true});
  expect(mocks.send).not.toHaveBeenCalled();fireEvent.compositionEnd(input);await new Promise(r=>setTimeout(r,35));
  await user.click(screen.getByRole('button',{name:'Send message'}));expect((await screen.findByRole('alert')).textContent).toContain('temporary');
  expect((input as HTMLTextAreaElement).value).toBe('  日本語  ');
  await user.click(screen.getByRole('button',{name:'Send message'}));
  expect(mocks.send).toHaveBeenCalledTimes(2);expect(mocks.send.mock.calls[0]![0]).toBe(mocks.send.mock.calls[1]![0]);
  expect(mocks.send.mock.calls[0]!.slice(1,4)).toEqual(['dev',null,'  日本語  ']);
  expect((input as HTMLTextAreaElement).value).toBe('');expect(mocks.reveal).toHaveBeenCalledWith('dev',null,'own-post');
});
test('an in-flight completion clears only its original destination while another draft survives',async()=>{
  let resolve!:(value:unknown)=>void;mocks.send.mockImplementation(()=>new Promise(r=>{resolve=r;}));
  const view=render(composer());const user=userEvent.setup();await user.type(screen.getByRole('textbox'),'first');
  await user.click(screen.getByRole('button',{name:'Send message'}));
  view.rerender(<DraftProvider><Composer key="design" channel="design" root={null} ready handles={[]}/></DraftProvider>);
  await user.type(screen.getByRole('textbox'),'second');await act(async()=>resolve({id:'first-id',warnings:[]}));
  expect((screen.getByRole('textbox') as HTMLTextAreaElement).value).toBe('second');
  expect(mocks.reveal).toHaveBeenCalledWith('dev',null,'first-id');
});
test('draft capacity cannot evict nonempty or pending work',()=>{
  let drafts=new Map();for(let i=0;i<32;i++)drafts=new Map(draftReducer(drafts,{key:String(i),update:{text:'unsent'}}));
  expect(()=>draftReducer(drafts,{key:'overflow',update:{text:'new'}})).toThrow();
  drafts=new Map(draftReducer(drafts,{key:'0',update:{text:'',pending:false}}));
  expect(draftReducer(drafts,{key:'new',update:{text:'new'}}).size).toBe(32);
});
