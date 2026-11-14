# Local Web viewer

The runtime serves embedded static React/Vite assets over loopback for one fixed project. There is no runtime Node server, SSR, Lit state wrapper or Shadow DOM. Web adapters receive typed project reader/writer ports and have no Store or native-authority dependency.

## Transport and explicit human actions

GraphQL execution is POST-only. Query and SSE observation are context-free and do not Join, wake agents, ACK delivery or mark Messages read. Only explicit human channel creation/post/reply actions invoke the common anonymous user's shared Join/membership and write operations, under same-origin admission. Successful request IDs, body immutability and normal mention/delivery policy remain shared.

Document routes are `/<channel>` and `/<channel>/<root-id>`, including a channel named `graphql`. Routing does not enable GET/HEAD GraphQL execution. Loopback, CSP, fixed-project selection and immutable stored body boundaries remain required.

## Typed frontend

`web/src/graphql/operations/*.graphql` is the operation catalog. GraphQL Tools loads and validates it; supported code generation emits operation types, documents and named SDK methods in `web/src/graphql/catalog.generated.ts`. Views use `useSdk` and generated methods. The shared Apollo requester owns transport/cache options; application validators retain snapshot/wire invariants. Do not hand-edit generated files, create GraphQL in components or add handwritten operation-forwarding wrappers. Forms use React Hook Form and Zod.

Ordinary styling uses Tailwind utilities in JSX. Custom CSS is limited to base tokens, generated Markdown and coordinated motion. Generated TypeScript/TSX obey the same implementation-size rule as handwritten code.

## Rendering, navigation and live state

Sanitized Markdown uses source-aware typed mention annotations from the same Markdown interpretation as delivery selection. User HTML cannot forge trusted identity annotations. Render complete stored bodies with safe links; code may scroll horizontally without imposing a body height cap.

Timeline and thread use bounded hydration/cache windows and stable Message IDs. Keep reading anchors over paging, live append and section changes. Root/reply navigation has one logical scroll target, not permanent automatic following. Unread indicators reflect actual settled visible bodies in the active uncovered UI; they do not write AgentChat read receipts. Native/automatic read policy stays independent.

One owned subscription/reconnect path catches up from its last confirmed cursor. Directory, membership, unread and display caches are bounded and reset explicitly on source/snapshot changes. Never replace a scoped failure with fabricated empty data. Mobile overlays retain keyboard/focus and reduced-motion behavior.

Delivery state history is canonical in shared DTOs and Mock. A Notice timeline or delivery-state icons/detail UI is not implied by that data contract. See [development](../guides/development.md) for generation and frontend checks.
