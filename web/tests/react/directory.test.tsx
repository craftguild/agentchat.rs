// @vitest-environment jsdom
import {test,expect,vi,afterEach} from 'vitest';
import {renderHook,act,cleanup} from '@testing-library/react';
import {useDirectory} from '../../src/state/directory.ts';
import {fixture} from './fixtures.ts';
const mocks=vi.hoisted(()=>({channels:vi.fn(),agents:vi.fn(),members:vi.fn()}));
vi.mock('../../src/graphql/provider.tsx',()=>{
  const sdk={
    Channels:(v,o)=>mocks.channels(v.after,o.signal).then(channels=>({channels})),
    Agents:(v,o)=>mocks.agents(v.after,o.signal).then(agents=>({agents})),
    Members:(v,o)=>mocks.members(v.channel,v.after,o.signal).then(channelMembers=>({channelMembers})),
  };return {useSdk:()=>({sdk})};
});
afterEach(()=>{cleanup();vi.resetAllMocks();vi.useRealTimers();});
const page=(name='dev')=>({...fixture.channels,items:[{...fixture.channels.items[0],name}],asOf:'2026-10-01T00:00:00.000Z'});
const otherReads=()=>{mocks.agents.mockResolvedValue({...fixture.agents,items:[],nextCursor:null,scanCursor:'1',head:'1'});mocks.members.mockResolvedValue({items:[],nextCursor:null,scanCursor:'1',head:'1'});};
const flush=()=>act(async()=>{for(let i=0;i<30;i++)await Promise.resolve();});
test('a coalesced refresh awaits the newer directory before create navigation can continue',async()=>{
  otherReads();const resolve:((value:unknown)=>void)[]=[];
  mocks.channels.mockImplementation(()=>new Promise(r=>resolve.push(r)));
  const view=renderHook(()=>useDirectory(true,'',0));await flush();expect(resolve).toHaveLength(1);
  let completed=false,pending!:Promise<void>;act(()=>{pending=view.result.current.refreshChannels().then(()=>{completed=true;});});
  await act(async()=>resolve[0]!(page()));await flush();expect(resolve).toHaveLength(2);expect(completed).toBe(false);
  await act(async()=>{resolve[1]!(page('created-room'));await pending;});expect(completed).toBe(true);expect(view.result.current.channels[0]!.name).toBe('created-room');
});
test('reset cancels old directory ownership and a late response cannot replace the new epoch',async()=>{
  otherReads();let first!:(v:unknown)=>void;const signals:AbortSignal[]=[];
  mocks.channels.mockImplementation((_after,signal)=>{signals.push(signal);return signals.length===1?new Promise(r=>first=r):Promise.resolve(page('current-room'));});
  const view=renderHook(({enabled})=>useDirectory(enabled,'',0),{initialProps:{enabled:true}});await flush();view.rerender({enabled:false});await flush();expect(signals[0]!.aborted).toBe(true);
  view.rerender({enabled:true});await flush();await act(async()=>first(page('obsolete-room')));expect(view.result.current.channels[0]!.name).toBe('current-room');
});
test('archive deadlines chunk the platform timer limit and never poll the API',async()=>{
  vi.useFakeTimers();otherReads();const value=page();value.items[0]={...value.items[0],archived:false,archiveAt:'2026-10-31T00:00:00.000Z'};mocks.channels.mockResolvedValue(value);
  const view=renderHook(()=>useDirectory(true,'',0));await flush();expect(view.result.current.channels[0]!.archived).toBe(false);
  await act(()=>vi.advanceTimersByTimeAsync(2147483647));expect(view.result.current.channels[0]!.archived).toBe(false);
  await act(()=>vi.advanceTimersByTimeAsync(30*86400000-2147483647));expect(view.result.current.channels[0]!.archived).toBe(true);expect(mocks.channels).toHaveBeenCalledTimes(1);
});
