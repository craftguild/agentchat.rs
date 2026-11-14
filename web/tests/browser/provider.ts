/** Test-only data control uses stdin; the HTTP API remains the product surface. */
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export class Provider {
  url = '';
  private child;
  private output = '';
  private diagnostic = '';
  private pending: { resolve:(value:any)=>void; reject:(error:Error)=>void; timer:ReturnType<typeof setTimeout> }[] = [];
  private ready: (url: string) => void = () => {};
  private exit: Promise<number | null>;
  private failure: (error: Error) => void = () => {};
  constructor(backend: 'fixture' | 'real' = 'fixture') {
    const root = fileURLToPath(new URL('../../../', import.meta.url));
    const binary = backend === 'real' ? process.env.AGENTCHAT_REAL_BROWSER_BINARY : process.env.AGENTCHAT_WEB_BROWSER_BINARY;
    const entry = backend === 'real' ? 'real_browser_provider' : 'browser_provider';
    const command = binary ? [binary,entry,'--ignored','--exact','--nocapture']
      : ['cargo','test','--locked','--offline','-p',backend === 'real' ? 'agentchat-cli' : 'agentchat-web',
        '--test',entry,entry,'--','--ignored','--exact','--nocapture'];
    this.child = spawn('python3', ['tools/test-isolated.py', ...command],
      { cwd:root, detached:true, env:{ ...process.env, AGENTCHAT_WEB_BROWSER_PROVIDER:'1' }, stdio:['pipe','pipe','pipe'] });
    this.child.stdout.setEncoding('utf8'); this.child.stderr.setEncoding('utf8');
    this.child.stdout.on('data', chunk => {
      this.output += chunk;
      let end;
      while ((end = this.output.indexOf('\n')) >= 0) {
        const line = this.output.slice(0,end); this.output = this.output.slice(end+1);
        if (line.startsWith('AGENTCHAT_BROWSER_URL=')) this.ready(line.split('=')[1]!);
        else if (line.startsWith('AGENTCHAT_BROWSER_CONTROL=')) {
          const pending = this.pending.shift();
          if (pending) { clearTimeout(pending.timer); pending.resolve(JSON.parse(line.slice(line.indexOf('=')+1))); }
        }
        else this.diagnostic = (this.diagnostic + line + '\n').slice(-32_000);
      }
    });
    this.child.stderr.on('data', chunk => { this.diagnostic = (this.diagnostic + chunk).slice(-32_000); });
    this.child.stdin.on('error', error => this.fail(error));
    this.child.on('error', error => this.fail(error));
    this.exit = new Promise(resolve => {
      this.child.on('exit', code => { this.fail(new Error(`Browser provider exited ${code}: ${this.diagnostic}`)); resolve(code); });
      this.child.on('error', () => resolve(-1));
    });
  }
  async start(): Promise<void> {
    this.url = await new Promise((resolve, reject) => {
      this.ready = resolve;
      const timeout = setTimeout(() => reject(new Error(`Browser provider startup timeout: ${this.diagnostic}`)), 300_000);
      this.failure = error => { clearTimeout(timeout); reject(error); };
      this.ready = url => { clearTimeout(timeout); resolve(url); };
    });
  }
  async control(command: object): Promise<any> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`Control timeout: ${this.diagnostic}`)), 15_000);
      this.pending.push({resolve,reject,timer}); this.child.stdin.write(JSON.stringify(command) + '\n');
    });
  }
  async close(): Promise<void> {
    this.failure = () => {};
    if (this.child.exitCode === null) this.child.stdin.end(JSON.stringify({command:'stop'}) + '\n');
    const exited = async (milliseconds: number) => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      try { return await Promise.race([this.exit.then(code => ({code})),
        new Promise<null>(resolve => { timer = setTimeout(() => resolve(null), milliseconds); })]); }
      finally { clearTimeout(timer); }
    };
    let result = await exited(12_000);
    if (!result) {
      // The detached group contains only this owned fixture and its CLI child.
      // Never leave a viewer behind if startup or the stdin controller stalls.
      try { process.kill(-this.child.pid!, 'SIGTERM'); } catch {}
      result = await exited(5_000);
      if (!result) { try { process.kill(-this.child.pid!, 'SIGKILL'); } catch {} await this.exit; }
      throw new Error(`Browser provider exceeded its cleanup deadline: ${this.diagnostic}`);
    }
    const {code} = result;
    if (code !== 0) throw new Error(this.diagnostic);
  }
  private fail(error: Error): void {
    this.failure(error);
    for (const pending of this.pending.splice(0)) { clearTimeout(pending.timer); pending.reject(error); }
  }
}
