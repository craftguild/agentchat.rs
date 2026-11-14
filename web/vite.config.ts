import { defineConfig } from 'vite';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';
import { realpathSync } from 'node:fs';
import { loadNotices, npmPackage } from './scripts/notices.mjs';
import { catalogPlugin } from './scripts/graphql.mjs';
import { bundleEvidence } from './scripts/bundle-evidence.mjs';

const web = fileURLToPath(new URL('.', import.meta.url));
const owned = [web + 'src/', web + 'node_modules/', web + 'index.html'];
const notices = await loadNotices();

export default defineConfig({
  plugins: [{
    name: 'agentchat-owned-browser-modules', apply: 'build', enforce: 'pre',
    transform(_code, id) {
      const path = id.split('?')[0]!;
      if (path.startsWith('/') && !owned.some(prefix => realpathSync(path).startsWith(prefix))) {
        this.error('Viewer bundles may only import owned frontend files and locked npm dependencies.');
      }
      const dependency = npmPackage(path);
      if (dependency && !notices.names.has(dependency)) this.error(`Missing viewer notice: ${dependency}`);
    },
    generateBundle() {
      this.emitFile({ type:'asset', fileName:'third-party-notices.txt', source:notices.text });
    },
  }, {
    name: 'agentchat-dev-notices', apply: 'serve',
    configureServer(server) {
      server.middlewares.use('/third-party-notices.txt', (_request, response) => {
        response.setHeader('Content-Type', 'text/plain; charset=utf-8');
        response.end(notices.text);
      });
    },
  }, catalogPlugin(), react(), tailwindcss(), bundleEvidence()],
  build: {
    target: 'es2022',
    assetsInlineLimit: 0,
    sourcemap: false,
    reportCompressedSize: false,
  },
  server: { host: '127.0.0.1' },
});
