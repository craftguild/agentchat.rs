# Viewer development

The production Viewer is a static embedded React application. Development uses Vite and a temporary Chigiri Mock project; it does not alter real AgentChat state.

## Start the development UI

Use Node.js 24.15.0 or newer and npm 11.12.1. Supply a compatible Chigiri CLI with Mock project v9 and the generated subscribe/publish API through `CHIGIRI_BIN`, or install that CLI on PATH as `chigiri`. Prepare that executable before starting the development controller.

```sh
npm ci --ignore-scripts
npm run dev
```

An explicit executable can be selected with `CHIGIRI_BIN=/path/to/chigiri npm run dev`. The controller snapshots that file, starts its own Mock and Vite, prints the loopback URL and removes its temporary project on Ctrl-C. `AGENTCHAT_DEV_PORT` selects the UI port, default 5173. `npm run dev:post -- 'Example message'` posts to the owned development fixture. Restart development after fixture/schema changes.

## Types and checks

`src/graphql/operations/*.graphql` is the catalog. Run `npm run codegen` after changing operations and `npm run codegen:check` when verifying them. GraphQL Tools validates the catalog; generated named SDK methods use one Apollo requester. Views use `useSdk`; do not hand-edit generated code or write operation-forwarding SDK wrappers. React Hook Form/Zod own form validation.

```sh
npm run typecheck
npm test
npm run test:dev
npm run build
```

Browser and real-backend integration suites have separate fixtures and should be selected according to the changed boundary. Ordinary JSX uses Tailwind utilities; custom CSS is limited to base tokens, generated Markdown and coordinated motion.

`npm run build` emits static assets and their complete input/hash manifest for Rust embedding. Node is build-only. Keep the loopback/CSP/fixed-project boundary, typed read replay and explicit same-origin common-user writes. See [Viewer design](../docs/design/viewer-contract.md), [development](../docs/guides/development.md) and [Rust generator provenance](../tools/provenance/chigiri/README.md).
