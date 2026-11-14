#!/usr/bin/env python3
"""Probe actual Claude SDK idle rewake/isolation without admitting a host profile.

Runs two new, disposable root conversations with inherited host policies/auth.
Only task-owned --settings hooks and temporary agentchat roots are added. This is SDK
route evidence, not ordinary TUI startup, product delivery acknowledgement,
pause/approval preservation, or the complete G0 acceptance gate.
"""

import argparse
import json
import os
import pathlib
import selectors
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid


HOOK = r'''
import fcntl, json, os, pathlib, sys
root = pathlib.Path(sys.argv[1])
data = json.loads(sys.stdin.buffer.read(65537))
if data.get('agent_id'):
    raise SystemExit(0)
with (root/'hook.lock').open('a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    event = {key:data.get(key) for key in ('hook_event_name','session_id','cwd','source','event','file_path')}
    with (root/'hooks.jsonl').open('a') as log:
        log.write(json.dumps(event)+'\n')
        log.flush()
        os.fsync(log.fileno())
    if data.get('hook_event_name') != 'FileChanged':
        raise SystemExit(0)
    pending = root/'pending.json'
    if not pending.exists():
        raise SystemExit(0)
    intent = json.loads(pending.read_text())
    if data.get('session_id') != intent['target_session']:
        raise SystemExit(0)
    receipt = root/('signalled-'+data['session_id'])
    if receipt.exists() and int(receipt.read_text()) >= intent['generation']:
        raise SystemExit(0)
    receipt.write_text(str(intent['generation']))
    print('Disposable agentchat host probe: output exactly '+intent['reply_marker']+'. Do not use any tools.', file=sys.stderr)
    raise SystemExit(2)
'''


class Conversation:
    """One test-owned subprocess; no discovery of existing conversations."""

    def __init__(self, executable, root, project, settings, selector, resume=None, fork=False):
        self.session_id = resume if resume and not fork else str(uuid.uuid4())
        self.stopped = False
        self.selector = selector
        self.messages = []
        self.results = []
        self.buffers = {}
        env = os.environ.copy()
        env['AGENTCHAT_HOME'] = str(root/'state')
        env['AGENTCHAT_USER_CONFIG_ROOT'] = str(root/'config')
        args = [executable, '-p', '--input-format', 'stream-json',
                '--output-format', 'stream-json', '--verbose', '--settings', str(settings)]
        if resume:
            args.extend(['--resume', resume])
        if not resume or fork:
            args.extend(['--session-id', self.session_id])
        if fork:
            args.append('--fork-session')
        self.child = subprocess.Popen(args, cwd=project, env=env, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                      start_new_session=True)
        self.log = (root/(self.session_id+'.jsonl')).open('a')
        for stream, kind in [(self.child.stdout, 'stdout'), (self.child.stderr, 'stderr')]:
            self.buffers[kind] = bytearray()
            selector.register(stream, selectors.EVENT_READ, (self, kind))

    def send(self, prompt):
        data = {'type':'user', 'session_id':self.session_id,
                'message':{'role':'user','content':prompt}, 'parent_tool_use_id':None}
        self.child.stdin.write((json.dumps(data)+'\n').encode())
        self.child.stdin.flush()

    def read(self, stream, kind, selector):
        chunk = os.read(stream.fileno(), 8192)
        if not chunk:
            selector.unregister(stream)
            return
        buffer = self.buffers[kind]
        buffer.extend(chunk)
        if len(buffer) > 1_048_576:
            raise RuntimeError('Host stream exceeded the bounded frame budget.')
        while b'\n' in buffer:
            raw, _, tail = buffer.partition(b'\n')
            self.buffers[kind] = buffer = bytearray(tail)
            if kind != 'stdout':
                continue
            try:
                value = json.loads(raw)
            except ValueError:
                continue
            # Keep only fixture model text and lifecycle metadata. Never dump
            # init configuration, tool credentials, account details or stderr.
            record = {'type':value.get('type'), 'session_id':value.get('session_id')}
            if value.get('type') == 'assistant':
                text = ''.join(block.get('text','') for block in value.get('message',{}).get('content',[])
                               if block.get('type') == 'text')
                self.messages.append(text)
                record['text'] = text
            if value.get('type') == 'result':
                self.results.append(value)
                record.update(is_error=value.get('is_error'), subtype=value.get('subtype'))
            self.log.write(json.dumps(record)+'\n')
            self.log.flush()

    def stop(self):
        if self.stopped:
            return
        self.stopped = True
        self.child.stdin.close()
        try:
            self.child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(self.child.pid, signal.SIGTERM)
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(self.child.pid, signal.SIGKILL)
                self.child.wait()
        for stream in (self.child.stdout, self.child.stderr):
            try:
                self.selector.unregister(stream)
            except KeyError:
                pass
            stream.close()
        self.log.close()


def pump(selector, predicate, timeout, conversations):
    deadline = time.monotonic()+timeout
    while not predicate():
        for key, _ in selector.select(min(0.2, max(0, deadline-time.monotonic()))):
            conversation, kind = key.data
            conversation.read(key.fileobj, kind, selector)
        if any(not c.stopped and c.child.poll() is not None for c in conversations):
            raise RuntimeError('A test-owned Claude process exited before the scenario completed.')
        if time.monotonic() >= deadline:
            raise TimeoutError('Timed out waiting for actual model/lifecycle evidence.')


def hooks(root):
    path = root/'hooks.jsonl'
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def write_signal(path, generation):
    inode = path.stat().st_ino
    with path.open('r+') as signal_file:
        signal_file.write(str(generation)+'\n')
        signal_file.truncate()
        signal_file.flush()
        os.fsync(signal_file.fileno())
    assert path.stat().st_ino == inode, 'Signal update replaced its inode.'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--claude', default=shutil.which('claude'))
    parser.add_argument('--output-parent', type=pathlib.Path)
    parser.add_argument('--timeout', type=float, default=90)
    args = parser.parse_args()
    if not args.claude:
        parser.error('Pass --claude with an installed authenticated executable.')
    root = pathlib.Path(tempfile.mkdtemp(prefix='agentchat-g0-claude-', dir=args.output_parent)).resolve()
    for name in ('state','config','project'):
        (root/name).mkdir(mode=0o700)
    project = root/'project'
    subprocess.run(['git','init','--quiet'], cwd=project, check=True)
    signal_path = project/'.agentchat-wakeup'
    signal_path.write_text('0\n')
    script = root/'hook.py'
    script.write_text(HOOK)
    command = ' '.join(shlex.quote(str(p)) for p in (sys.executable, script, root))
    handler = {'type':'command','command':command,'timeout':10}
    settings = root/'probe-settings.json'
    settings.write_text(json.dumps({'hooks':{
        'SessionStart':[{'hooks':[handler]}], 'SessionEnd':[{'hooks':[handler]}],
        'Stop':[{'hooks':[handler]}],
        'FileChanged':[{'matcher':'.agentchat-wakeup','hooks':[{**handler,'asyncRewake':True}]}],
    }}))
    report = {'root':str(root), 'status':'running', 'g0_admitted':False,
              'route':'print/stream-json; ordinary TUI and product acknowledgement untested',
              'checks':{}, 'host_version':subprocess.check_output([args.claude,'--version'], text=True).strip()}
    selector = selectors.DefaultSelector()
    conversations = []
    try:
        for label in ('a','b'):
            conversation = Conversation(args.claude, root, project, settings, selector)
            conversations.append(conversation)
            report[label] = {'session_id':conversation.session_id,'pid':conversation.child.pid}
            conversation.send('This is a disposable integration probe. Do not use tools, change files, or launch agents. Output exactly AGENTCHAT_INITIAL_'+label+'.')
        (root/'report.json').write_text(json.dumps(report,indent=2))
        print(json.dumps(report), flush=True)
        pump(selector, lambda:all(c.results for c in conversations), args.timeout, conversations)
        for label, conversation in zip(('a','b'), conversations):
            assert not conversation.results[0].get('is_error'), 'Initial model turn failed.'
            assert 'AGENTCHAT_INITIAL_'+label in ''.join(conversation.messages), 'Missing real initial model output.'
            assert any(e['hook_event_name']=='SessionStart' and e['session_id']==conversation.session_id
                       and e['cwd']==str(project) for e in hooks(root)), 'Exact root lifecycle payload missing.'
        report['checks']['two_exact_same_directory_roots'] = 'passed'
        a, b = conversations
        original_b = len(b.messages)
        for generation in range(1, 4):
            marker = 'AGENTCHAT_WAKE_'+uuid.uuid4().hex
            (root/'pending.json').write_text(json.dumps({'target_session':a.session_id,
                'generation':generation,'reply_marker':marker}))
            write_signal(signal_path, generation)
            pump(selector, lambda:any(marker in text for text in a.messages), args.timeout, conversations)
            quiet_until = time.monotonic()+2
            pump(selector, lambda:time.monotonic()>=quiet_until, 3, conversations)
            assert len(b.messages)==original_b, 'Untargeted same-directory conversation invoked its model.'
            assert any(e['hook_event_name']=='FileChanged' and e['session_id']==b.session_id for e in hooks(root)), 'Untargeted hook was not observed.'
            report['checks']['idle_rewake_generation_'+str(generation)] = 'passed'
            (root/'report.json').write_text(json.dumps(report,indent=2))
            print(json.dumps({'generation':generation,'targeted_model_receipt':True,'untargeted_model_unchanged':True}), flush=True)
        a.stop()
        assert any(e['hook_event_name']=='SessionEnd' and e['session_id']==a.session_id for e in hooks(root)), 'Normal exit lifecycle payload missing.'
        resumed = Conversation(args.claude, root, project, settings, selector, resume=a.session_id)
        conversations.append(resumed)
        report['resume'] = {'session_id':resumed.session_id,'pid':resumed.child.pid}
        resumed.send('Disposable resume probe. Do not use tools or change files. Output exactly AGENTCHAT_RESUMED.')
        pump(selector, lambda:bool(resumed.results), args.timeout, conversations)
        assert not resumed.results[0].get('is_error'), 'Resumed model turn failed.'
        assert resumed.results[0].get('session_id')==a.session_id, 'Resume changed the root conversation ID.'
        assert any(e['hook_event_name']=='SessionStart' and e['session_id']==a.session_id
                   and e['source']=='resume' for e in hooks(root)), 'Exact resume lifecycle payload missing.'
        report['checks']['normal_exit_and_same_root_resume'] = 'passed'
        resumed.stop()
        fork = Conversation(args.claude, root, project, settings, selector, resume=a.session_id, fork=True)
        conversations.append(fork)
        report['fork'] = {'session_id':fork.session_id,'pid':fork.child.pid}
        fork.send('Disposable fork probe. Do not use tools or change files. Output exactly AGENTCHAT_FORKED.')
        pump(selector, lambda:bool(fork.results), args.timeout, conversations)
        assert not fork.results[0].get('is_error'), 'Forked model turn failed.'
        assert fork.results[0].get('session_id')==fork.session_id!=a.session_id, 'Fork did not have its separate root ID.'
        assert any(e['hook_event_name']=='SessionStart' and e['session_id']==fork.session_id for e in hooks(root)), 'Fork lifecycle root payload missing.'
        marker = 'AGENTCHAT_OFFLINE_'+uuid.uuid4().hex
        before_fork = len(fork.messages)
        (root/'pending.json').write_text(json.dumps({'target_session':a.session_id,'generation':4,'reply_marker':marker}))
        write_signal(signal_path, 4)
        quiet_until = time.monotonic()+3
        pump(selector, lambda:time.monotonic()>=quiet_until, 4, conversations)
        assert len(fork.messages)==before_fork and len(b.messages)==original_b, 'A different root received the offline original root probe.'
        assert any(e['hook_event_name']=='FileChanged' and e['session_id']==fork.session_id for e in hooks(root)), 'Fork FileChanged hook was not observed.'
        report['checks']['fork_root_and_offline_target_isolation'] = 'passed'
        report['status'] = 'passed_partial_probe'
    except (AssertionError, RuntimeError, TimeoutError, OSError) as error:
        report.update(status='failed', error=str(error))
    finally:
        for conversation in conversations:
            conversation.stop()
        selector.close()
        report['hook_counts'] = {c.session_id:sum(e['session_id']==c.session_id for e in hooks(root)) for c in conversations}
        (root/'report.json').write_text(json.dumps(report,indent=2))
        print(json.dumps(report), flush=True)
    return 0 if report['status']=='passed_partial_probe' else 1


if __name__ == '__main__':
    sys.exit(main())
