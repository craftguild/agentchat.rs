import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createServer} from 'node:net';
export async function freePort():Promise<number>{const listener=createServer();listener.listen(0,'127.0.0.1');await once(listener,'listening');const port=(listener.address() as any).port;await new Promise<void>(r=>listener.close(()=>r()));return port;}
export async function startDev(extra:Record<string,string>={}){
  const port=await freePort(),origin=`http://127.0.0.1:${port}`;
  const child=spawn(process.execPath,['scripts/dev.mjs'],{env:{...process.env,AGENTCHAT_DEV_PORT:String(port),...extra},stdio:['ignore','pipe','pipe']});
  let output='';for(const stream of [child.stdout,child.stderr])stream.on('data',data=>{output=(output+data).slice(-64000);});const done=once(child,'close');
  async function close(){if(child.exitCode===null)child.kill('SIGINT');await done;}
  try{for(let i=0;i<600&&!output.includes('Viewer: ');i++){assert.equal(child.exitCode,null,output);await new Promise(r=>setTimeout(r,50));}assert.match(output,/Viewer: /);return {origin,close,output:()=>output,child};}catch(error){await close();throw error;}
}
