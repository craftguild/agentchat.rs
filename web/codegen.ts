import type { CodegenConfig } from '@graphql-codegen/cli';
import { loadSchemaSync, loadDocumentsSync } from '@graphql-tools/load';
import { GraphQLFileLoader } from '@graphql-tools/graphql-file-loader';
import { concatAST, validate, specifiedRules, NoUnusedFragmentsRule } from 'graphql';

const loaders = [new GraphQLFileLoader()];
const schema=loadSchemaSync(['graphql/schema.graphql','src/graphql/cache.graphql'],{loaders});
const sources=loadDocumentsSync('src/graphql/operations/*.graphql',{loaders});
const errors=validate(schema,concatAST(sources.map(s=>s.document!)),specifiedRules.filter(rule=>rule!==NoUnusedFragmentsRule));
if(errors.length)throw new Error(errors.map(e=>e.message).join('\n'));
/** The catalog generates the complete operation SDK; Apollo supplies its requester. */
const config: CodegenConfig = {
  schema,
  documents:'src/graphql/operations/*.graphql',
  generates:{'src/graphql/catalog.generated.ts':{plugins:['typescript-operations','typescript-generic-sdk']}},
  config: {
    documentMode:'documentNode',
    usingObservableFrom:"import type { Observable } from 'rxjs';",
    useTypeImports:true, enumsAsTypes:true, skipTypename:true,
    avoidOptionals:{field:true,object:true},
    strictScalars:true, scalars:{ID:{input:'string',output:'string'}},
  },
};
export default config;
