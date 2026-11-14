import { createContext, useContext, useMemo } from 'react';
import type { ReactNode } from 'react';
import { ApolloProvider } from '@apollo/client/react';
import { createViewerClient, createContentCache } from './client.ts';
import { createSdk } from './sdk.ts';

function resources(){const client=createViewerClient();return {client,sdk:createSdk(client),contents:createContentCache(client)};}
export type ViewerResources=ReturnType<typeof resources>;
const Context=createContext<ViewerResources|null>(null);
/** A document owns one Apollo cache and SDK, shared by every pane and form. */
export function ViewerProvider({children}:{children:ReactNode}) {
  const value=useMemo(resources,[]);
  return <ApolloProvider client={value.client}><Context.Provider value={value}>{children}</Context.Provider></ApolloProvider>;
}
/** Typed operation access; views never construct queries or call fetch. */
export function useSdk():ViewerResources {
  const value=useContext(Context);if(!value)throw new Error('Missing ViewerProvider');return value;
}
