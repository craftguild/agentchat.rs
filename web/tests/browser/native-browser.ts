/** An owned Chrome profile without Playwright's forced-visible page defaults. */
import { chromium } from '@playwright/test';
import type { Browser } from '@playwright/test';
import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';

export class NativeBrowser {
  browser: Browser | null = null;
  private profile = '';
  private child: ReturnType<typeof spawn> | null = null;
  private exited: Promise<void> = Promise.resolve();

  async start(headless = false): Promise<Browser> {
    const executable = process.env.AGENTCHAT_NATIVE_BROWSER_BINARY ?? chromium.executablePath();
    if (!isAbsolute(executable)) throw new Error('Native browser requires an absolute executable path');
    this.profile = await mkdtemp(join(tmpdir(), 'agentchat-native-browser-'));
    this.child = spawn(executable, [
      '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0',
      `--user-data-dir=${this.profile}`, '--no-first-run', '--no-default-browser-check',
      ...(headless ? ['--headless=new'] : []), 'about:blank',
    ], { stdio:['ignore','ignore','pipe'] });
    const child = this.child;
    this.exited = new Promise(resolve => child.once('exit', () => resolve()));
    try {
      const endpoint = await new Promise<string>((resolve, reject) => {
        let diagnostic = '';
        const timer = setTimeout(() => reject(new Error('Owned Chrome startup: ' + diagnostic)), 30_000);
        child.stderr!.on('data', chunk => {
          diagnostic = (diagnostic + chunk).slice(-32_000);
          const match = diagnostic.match(/DevTools listening on (ws:\/\/[^\s]+)/);
          if (match) { clearTimeout(timer); resolve(match[1]!); }
        });
        child.once('error', error => { clearTimeout(timer); reject(error); });
        child.once('exit', (code, signal) => {
          clearTimeout(timer);
          reject(new Error(`Owned Chrome exited code=${code} signal=${signal}: ${diagnostic}`));
        });
      });
      if (new URL(endpoint).hostname !== '127.0.0.1') throw new Error('Debugging must stay loopback-only');
      // The public noDefaults option leaves native tab visibility intact. Normal
      // launch/newContext enables per-session focus emulation, even when another
      // session disables it. No document property or DOM event is replaced.
      this.browser = await chromium.connectOverCDP(endpoint, { noDefaults:true });
      return this.browser;
    } catch (error) { await this.close(); throw error; }
  }
  async close(): Promise<void> {
    try {
      if (this.browser?.isConnected()) {
        const session = await this.browser.newBrowserCDPSession();
        await session.send('Browser.close').catch(() => {});
        await this.browser.close().catch(() => {});
      }
    } finally {
      const child = this.child;
      if (child && child.exitCode === null && child.signalCode === null) {
        const timer = setTimeout(() => child.kill('SIGTERM'), 3000);
        await this.exited; clearTimeout(timer);
      }
      if (this.profile) await rm(this.profile, { recursive:true, force:true });
    }
  }
}
