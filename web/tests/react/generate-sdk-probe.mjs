/** Exercise the real Node generator realm, matching npm run codegen. */
import assert from 'node:assert/strict';
import { generate } from '@graphql-codegen/cli';
import { mkdtemp,writeFile,rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { stripTypeScriptTypes } from 'node:module';
import { getOperationAST } from 'graphql';
import config from '../../codegen.ts';
const directory=await mkdtemp(join(tmpdir(),'agentchat-sdk-catalog-'));
try{
  const probe=join(directory,'probe.graphql');
  await writeFile(probe,'query CatalogProbe($after: String) { channels(after: $after) { head } }');
  const files=await generate({...config,documents:[config.documents,probe],silent:true},false);
  assert.equal(files.length,1);
  const content=files[0].content;assert(content.includes('CatalogProbe(variables?: CatalogProbeQueryVariables'));
  const js=stripTypeScriptTypes(content),module=await import(`data:text/javascript;base64,${Buffer.from(js).toString('base64')}`);
  const calls=[],sdk=module.getSdk((doc,variables)=>{
    calls.push({name:getOperationAST(doc)?.name?.value,variables});return Promise.resolve({channels:{head:'7'}});
  });
  assert.deepEqual(await sdk.CatalogProbe({after:'4'}),{channels:{head:'7'}});
  assert.deepEqual(calls,[{name:'CatalogProbe',variables:{after:'4'}}]);
  console.log('PASS: new catalog operation generates an executable SDK method');
}finally{await rm(directory,{recursive:true,force:true});}
