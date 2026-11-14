// @vitest-environment jsdom
import {afterEach,beforeEach,expect,test,vi} from 'vitest';
import {act,cleanup,render,screen} from '@testing-library/react';
import {Timeline} from '../../src/ui/timeline.tsx';
import {emptyTimeline} from '../../src/state/timeline.ts';
import {cp} from './fixtures.ts';

const mocks=vi.hoisted(()=>({options:null as any,position:{heights:new Map<string,number>(),anchor:{id:'retained',offset:8}}}));
vi.mock('@tanstack/react-virtual',()=>({useVirtualizer:(options:unknown)=>{
  mocks.options=options;return {getVirtualItems:()=>[],getTotalSize:()=>0,scrollToIndex:vi.fn(),measureElement:vi.fn()};
}}));
vi.mock('../../src/graphql/provider.tsx',()=>({useSdk:()=>({sdk:{},contents:new Map()})}));
vi.mock('../../src/state/session.tsx',()=>({useSession:()=>({reading:null,directory:{agents:[]},actions:{position:()=>mocks.position}})}));

let resized:()=>void,mutated:()=>void,next=0;
const frames=new Map<number,FrameRequestCallback>();
beforeEach(()=>{
  mocks.position.heights.clear();frames.clear();next=0;
  vi.stubGlobal('requestAnimationFrame',(callback:FrameRequestCallback)=>{frames.set(++next,callback);return next;});
  vi.stubGlobal('cancelAnimationFrame',(id:number)=>frames.delete(id));
  vi.stubGlobal('ResizeObserver',class {constructor(callback:()=>void){resized=callback;}observe(){}disconnect(){}});
  vi.stubGlobal('MutationObserver',class {constructor(callback:()=>void){mutated=callback;}observe(){}disconnect(){}});
});
afterEach(()=>{cleanup();vi.unstubAllGlobals();});

function mount(){
  const rendered=render(<Timeline view={emptyTimeline('dev',null,cp(0))} covered selectedRoot={null}/>);
  const list=screen.getByTestId('message-list');
  Object.defineProperty(list,'clientHeight',{value:300,configurable:true});
  list.getBoundingClientRect=()=>({top:100}) as DOMRect;
  const row=document.createElement('div'),article=document.createElement('article');
  row.dataset.id='retained';row.append(article);list.append(row);
  article.getBoundingClientRect=()=>({top:150-list.scrollTop}) as DOMRect;
  return {list,row,unmount:rendered.unmount};
}

test('resize, mutation and row measurement never correct scroll inside observer delivery',()=>{
  const {list,row}=mount();
  expect(mocks.options.useAnimationFrameWithResizeObserver).toBe(true);
  resized();mutated();
  expect(mocks.options.measureElement(row,{borderBoxSize:[{blockSize:140}]})).toBe(140);
  expect(mocks.position.heights.get('retained')).toBe(140);
  expect(list.scrollTop).toBe(0);
  expect(frames.size).toBe(1);
  const pending=[...frames.values()];frames.clear();act(()=>pending.forEach(callback=>callback(0)));
  expect(list.scrollTop).toBe(42);
});

test('unmount cancels a pending observer correction before it can touch the old list',()=>{
  const {list,unmount}=mount();resized();mutated();
  expect(list.scrollTop).toBe(0);expect(frames.size).toBe(1);
  unmount();expect(frames.size).toBe(0);expect(list.scrollTop).toBe(0);
});
