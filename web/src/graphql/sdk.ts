import type { ApolloClient } from '@apollo/client';
import { getSdk } from './catalog.generated.ts';
import { createApolloRequester } from './requester.ts';

/** Bind the catalog-generated SDK to this document's shared Apollo client. */
export function createSdk(client:ApolloClient){return getSdk(createApolloRequester(client));}
export type ViewerSdk=ReturnType<typeof createSdk>;
