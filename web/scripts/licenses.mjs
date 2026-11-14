/** Regenerate exact package notices after an intentional locked dependency update. */
import { readFile, writeFile, access } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
const web=fileURLToPath(new URL('..',import.meta.url));
const lock=JSON.parse(await readFile(resolve(web,'package-lock.json'),'utf8'));
const old=JSON.parse(await readFile(resolve(web,'licenses/packages.json'),'utf8'));
const entries=[];
for(const [path,version] of Object.entries(lock.packages)){
  if(!path || (version.dev && !['node_modules/vite','node_modules/tailwindcss'].includes(path)))continue;
  const pkg=JSON.parse(await readFile(resolve(web,path,'package.json'),'utf8'));
  if(path!==`node_modules/${pkg.name}`)throw new Error(`Nested runtime package needs an explicit notice path: ${path}`);
  const prior=old.find(e=>e.name===pkg.name && e.version===pkg.version);
  if(prior && prior.integrity===version.integrity && prior.license===pkg.license){
    const bytes=await readFile(resolve(web,prior.file));
    if(createHash('sha256').update(bytes).digest('hex')!==prior.sha256)throw new Error(`Changed retained notice: ${pkg.name}`);
    entries.push(prior);continue;
  }
  let file;
  for(const name of ['LICENSE','LICENSE.md','LICENSE.txt','license','license.md','LICENCE']){
    try{await access(resolve(web,path,name));file=`${path}/${name}`;break;}catch{}
  }
  const wry={ '@wry/context':'0.7.4','@wry/trie':'0.5.0' };
  const upstream=wry[pkg.name]===pkg.version ? 'https://raw.githubusercontent.com/benjamn/wryware/16493ef8b4fb994a17e64f2bbeaf22ae9be16b4a/LICENSE' : null;
  if(!file && upstream)file='licenses/wryware-MIT.txt';
  if(!file || typeof pkg.license!=='string')throw new Error(`Missing explicit license for ${pkg.name}`);
  entries.push({name:pkg.name,version:pkg.version,license:pkg.license,integrity:version.integrity,file,
    sha256:createHash('sha256').update(await readFile(resolve(web,file))).digest('hex'),
    source:upstream ?? 'npm package archive (package-lock.json integrity)',...(prior?.notice ? {notice:prior.notice} : {})});
}
entries.sort((a,b)=>a.name.localeCompare(b.name));
await writeFile(resolve(web,'licenses/packages.json'),JSON.stringify(entries,null,2)+'\n');
console.log(`Recorded ${entries.length} exact runtime/build notices.`);
