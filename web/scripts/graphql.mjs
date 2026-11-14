import { generate } from '@graphql-codegen/cli';
import { fileURLToPath } from 'node:url';
const configUrl=new URL('../codegen.ts',import.meta.url);
/** Read fresh catalog inputs for each serialized dev generation. */
export async function generateCatalog(){
  const {default:config}=await import(`${configUrl.href}?generation=${Date.now()}`);
  await generate({...config,silent:true},true);
}
/** Generated outputs cannot trigger their own watcher; errors reach Vite's overlay. */
export function catalogPlugin(){
  let building=false;
  return {name:'agentchat-graphql-catalog',
    configResolved(config){building=config.command==='build';},
    async buildStart(){if(building)await generateCatalog();},
    async configureServer(server){
      await generateCatalog();let tail=Promise.resolve();
      const directories=[fileURLToPath(new URL('../graphql',import.meta.url)),fileURLToPath(new URL('../src/graphql',import.meta.url))];
      server.watcher.add(directories);
      const changed=path=>{if(!path.endsWith('.graphql'))return;tail=tail.then(generateCatalog).catch(error=>{server.ws.send({type:'error',err:{message:error.message,stack:error.stack}});});};
      for(const event of ['change','add','unlink'])server.watcher.on(event,changed);
      server.httpServer?.once('close',()=>{for(const event of ['change','add','unlink'])server.watcher.off(event,changed);});
    },
  };
}
