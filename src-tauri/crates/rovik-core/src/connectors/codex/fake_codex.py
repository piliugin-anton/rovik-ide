#!/usr/bin/env python3
# A stand-in for `codex`, for tests (connectors::fake_codex).
#
# As `codex app-server` it speaks JSON-RPC on its stdio as Codex's app
# server does, and plays the turn SCENARIO gives, a step at a time: a
# message, its reasoning, a command of its shell or a patch (around which it
# calls Rovik's hooks over MCP, as Codex calls `mcp_tool` hooks, and asks
# the client where Codex would), a call of Rovik's tools, or a command that
# starts without its hooks. What it was asked, and what the hooks and the
# tools answered, goes to codex.json beside it. As `codex login status` it
# says it's signed in; as `codex --version`, its version. Each request of
# every run of it is also added to requests.jsonl, a line each.
import hashlib
import json
import os
import queue
import sys
import threading
import tomllib
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SCENARIO = json.loads(r'''__SCENARIO__''')
ARGS = sys.argv[1:]
RECORD = {'args': ARGS, 'requests': [], 'hooks': [], 'results': {}, 'applied': [],
          'approvals': [], 'listed': {}, 'env': {}}
for key in ['BASH_ENV', 'CODEX_HOME', 'CODEX_ACCESS_TOKEN', 'ROVIK_MCP_TOKEN', 'ROVIK_HOOKS_TOKEN']:
    RECORD['env'][key] = os.environ.get(key) is not None
if os.environ.get('BASH_ENV'):
    RECORD['bash_env'] = open(os.environ['BASH_ENV']).read()
LOCK = threading.Lock()


def save():
    with LOCK:
        with open(os.path.join(HERE, 'codex.json'), 'w') as f:
            json.dump(RECORD, f, indent=1)


if ARGS[:2] == ['login', 'status']:
    print('Logged in using ChatGPT', file=sys.stderr)
    sys.exit(0)
if ARGS[:1] == ['--version']:
    print('codex-cli 0.164.0')
    sys.exit(0)

# Its `-c key=value` settings, each value TOML, as Codex reads them.
CONFIG = {}
i = 0
while i < len(ARGS):
    if ARGS[i] == '-c' and i + 1 < len(ARGS):
        key, _, value = ARGS[i + 1].partition('=')
        CONFIG[key] = tomllib.loads('v = ' + value)['v']
        i += 2
    else:
        i += 1
HOOKS = {key[len('hooks.'):]: value for key, value in CONFIG.items() if key.startswith('hooks.')}
SNAKE = {'PreToolUse': 'pre_tool_use', 'PermissionRequest': 'permission_request', 'PostToolUse': 'post_tool_use'}
THREAD = None
TURN = None
POLICY = {}

OUT = threading.Lock()


def send(message):
    with OUT:
        sys.stdout.write(json.dumps(message) + '\n')
        sys.stdout.flush()


# The client's answers to the server's own requests, and its requests.
ANSWERS = {}
ANSWERED = threading.Condition()
INBOX = queue.Queue()


def read():
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if 'method' not in message and 'id' in message:
            with ANSWERED:
                ANSWERS[message['id']] = message
                ANSWERED.notify_all()
        else:
            INBOX.put(message)
    INBOX.put(None)


threading.Thread(target=read, daemon=True).start()
ASKED = [1000]


def ask_client(method, params):
    """A request of the server's (an approval): the client's answer."""
    with ANSWERED:
        ASKED[0] += 1
        rid = ASKED[0]
    send({'id': rid, 'method': method, 'params': dict(params, threadId=THREAD, turnId=TURN)})
    with ANSWERED:
        ANSWERED.wait_for(lambda: rid in ANSWERS, timeout=30)
        return ANSWERS.get(rid)


CALLS = [0]


def mcp(server, method, params):
    """A request to one of the run's MCP servers, with its token."""
    entry = CONFIG['mcp_servers.' + server]
    token = os.environ[entry['bearer_token_env_var']]
    CALLS[0] += 1
    body = {'jsonrpc': '2.0', 'id': CALLS[0], 'method': method, 'params': params}
    headers = {'Content-Type': 'application/json', 'Accept': 'application/json, text/event-stream',
               'Authorization': 'Bearer ' + token}
    request = urllib.request.Request(entry['url'], data=json.dumps(body).encode(), headers=headers)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=120) as response:
        text = response.read().decode()
    if text.lstrip().startswith('{'):
        return json.loads(text)
    for line in text.splitlines():
        if line.startswith('data: '):
            return json.loads(line[6:])


def fill(value, fields):
    """A hook's input with each `${field}` the event's, as it is."""
    if isinstance(value, dict):
        return {k: fill(v, fields) for k, v in value.items()}
    if isinstance(value, str) and value.startswith('${') and value.endswith('}'):
        name = value[2:-1]
        if name not in fields:
            sys.stderr.write(f'hook input placeholder `{value}` was not found\n')
            os._exit(3)
        return fields[name]
    return value


def hook(event, fields):
    """Rovik's hook for `event`, called as Codex calls an `mcp_tool` hook."""
    declared = HOOKS.get(event)
    if not declared:
        return None
    handler = declared[0]['hooks'][0]
    fields = dict(fields, hook_event_name=event, cwd=os.getcwd(), session_id=THREAD, turn_id=TURN)
    arguments = fill(handler['input'], fields)
    result = mcp(handler['server'], 'tools/call',
                 {'name': handler['tool'], 'arguments': arguments, '_meta': {'threadId': THREAD}})
    text = result['result']['content'][0]['text']
    answer = json.loads(text) if text.strip() else {}
    RECORD['hooks'].append({'event': event, 'id': fields.get('tool_use_id'),
                            'tool': fields.get('tool_name'), 'answer': answer})
    save()
    return answer


def output_of(answer):
    return (answer or {}).get('hookSpecificOutput') or {}


def denied(answer):
    out = output_of(answer)
    return out.get('permissionDecision') == 'deny' or (out.get('decision') or {}).get('behavior') == 'deny'


def updated_command(answer, command):
    out = output_of(answer)
    if out.get('permissionDecision') == 'allow' and isinstance(out.get('updatedInput'), dict):
        return out['updatedInput'].get('command', command)
    return command


def note(method, params):
    send({'method': method, 'params': dict(params, threadId=THREAD, turnId=TURN)})


def blocked(n):
    note('hook/completed', {'run': {'id': 'run-%d' % n, 'status': 'blocked'}})


def asks_before_running():
    """Whether Codex would ask the client before a call: no hook decides."""
    return POLICY.get('approvalPolicy') == 'untrusted' and 'PermissionRequest' not in HOOKS


def command_step(n, step):
    command, call = step['command'], step['id']
    pre = hook('PreToolUse', {'tool_name': 'Bash', 'tool_input': {'command': command}, 'tool_use_id': call})
    if denied(pre):
        return blocked(n)
    command = updated_command(pre, command)
    asked = hook('PermissionRequest', {'tool_name': 'Bash', 'tool_input': {'command': command}})
    if denied(asked):
        return blocked(n)
    if asks_before_running():
        answer = ask_client('item/commandExecution/requestApproval', {'itemId': call, 'command': command})
        decision = ((answer or {}).get('result') or {}).get('decision')
        RECORD['approvals'].append({'id': call, 'decision': decision})
        save()
        if decision not in ('accept', 'acceptForSession'):
            return
    item = {'type': 'commandExecution', 'id': call, 'command': "/bin/bash -lc '%s'" % command,
            'cwd': os.getcwd(), 'source': 'unifiedExecStartup', 'status': 'inProgress', 'commandActions': []}
    note('item/started', {'item': item})
    exit_code = step.get('exit', 0)
    output = step.get('output', '')
    done = dict(item, status='completed' if exit_code == 0 else 'failed', aggregatedOutput=output, exitCode=exit_code)
    note('item/completed', {'item': done})
    hook('PostToolUse', {'tool_name': 'Bash', 'tool_input': {'command': command}, 'tool_use_id': call,
                         'tool_response': output})


def patch_step(n, step):
    patch, call = step['patch'], step['id']
    pre = hook('PreToolUse', {'tool_name': 'apply_patch', 'tool_input': {'command': patch}, 'tool_use_id': call})
    if denied(pre):
        return blocked(n)
    patch = updated_command(pre, patch)
    changes = [{'path': os.path.join(os.getcwd(), path), 'kind': {'type': 'update', 'move_path': None},
                'diff': ''} for path in step.get('paths', [])]
    item = {'type': 'fileChange', 'id': call, 'changes': changes, 'status': 'inProgress'}
    note('item/started', {'item': item})
    asked = hook('PermissionRequest', {'tool_name': 'apply_patch', 'tool_input': {'command': patch}})
    if denied(asked):
        return note('item/completed', {'item': dict(item, status='declined')})
    RECORD['applied'].append(patch)
    save()
    note('item/completed', {'item': dict(item, status='completed')})


def rovik_step(step):
    """A call of Rovik's tools: it waits for Rovik's result, as Codex does."""
    call = step['id']
    item = {'type': 'mcpToolCall', 'id': call, 'server': 'rovik', 'tool': step['rovik'],
            'arguments': step.get('arguments', {}), 'status': 'inProgress'}
    note('item/started', {'item': item})
    result = mcp('rovik', 'tools/call', {'name': step['rovik'], 'arguments': step.get('arguments', {}),
                                         '_meta': {'callId': call, 'threadId': THREAD}})
    text = result['result']['content'][0]['text']
    RECORD['results'][call] = text
    save()
    note('item/completed', {'item': dict(item, status='completed', result={'content': [{'type': 'text', 'text': text}]})})


def text_step(n, text):
    for key, value in RECORD['results'].items():
        text = text.replace('{result:%s}' % key, value)
    item = {'type': 'agentMessage', 'id': 'm%d' % n, 'text': ''}
    note('item/started', {'item': item})
    note('item/agentMessage/delta', {'itemId': item['id'], 'delta': text})
    note('item/completed', {'item': dict(item, text=text)})


def play(steps):
    status, error = SCENARIO.get('status', 'completed'), SCENARIO.get('error')
    for n, step in enumerate(steps):
        if 'text' in step:
            text_step(n, step['text'])
        elif 'reasoning' in step:
            rid = 'r%d' % n
            note('item/started', {'item': {'type': 'reasoning', 'id': rid}})
            note('item/reasoning/summaryTextDelta', {'itemId': rid, 'delta': step['reasoning'], 'summaryIndex': 0})
            note('item/completed', {'item': {'type': 'reasoning', 'id': rid, 'summary': [step['reasoning']]}})
        elif 'plan' in step:
            pid = 'p%d' % n
            note('item/started', {'item': {'type': 'plan', 'id': pid, 'text': ''}})
            note('item/plan/delta', {'itemId': pid, 'delta': step['plan']})
            note('item/completed', {'item': {'type': 'plan', 'id': pid, 'text': step['plan']}})
        elif 'command' in step:
            command_step(n, step)
        elif 'patch' in step:
            patch_step(n, step)
        elif 'rovik' in step:
            rovik_step(step)
        elif 'unchecked' in step:
            # A command its hooks never saw: Rovik stops the turn.
            item = {'type': 'commandExecution', 'id': step['id'], 'command': step['unchecked'],
                    'cwd': os.getcwd(), 'source': 'unifiedExecStartup', 'status': 'inProgress'}
            note('item/started', {'item': item})
            status = 'interrupted'
            break
        elif 'usage' in step:
            inp, cached, out = step['usage']
            last = {'inputTokens': inp, 'cachedInputTokens': cached, 'cacheWriteInputTokens': 0,
                    'outputTokens': out, 'reasoningOutputTokens': 0, 'totalTokens': inp + out}
            note('thread/tokenUsage/updated', {'tokenUsage': {'total': last, 'last': last, 'modelContextWindow': 258400}})
    note('turn/completed', {'turn': {'id': TURN, 'status': status, 'error': error, 'items': []}})


def listed_hooks():
    hooks = []
    for event, declared in HOOKS.items():
        handler = declared[0]['hooks'][0]
        digest = hashlib.sha256(json.dumps(declared, sort_keys=True).encode()).hexdigest()
        hooks.append({'key': '/<session-flags>/config.toml:%s:0:0' % SNAKE.get(event, event),
                      'eventName': event[0].lower() + event[1:], 'handlerType': 'mcpTool',
                      'server': handler['server'], 'tool': handler['tool'], 'matcher': None,
                      'currentHash': 'sha256:' + digest, 'trustStatus': 'untrusted', 'enabled': True,
                      'source': 'sessionFlags'})
    return hooks


def trust(config):
    """The hooks the thread trusts run; the others don't, as with Codex."""
    global HOOKS
    state = (config or {}).get('hooks.state') or {}
    trusted = {}
    for entry in listed_hooks():
        if (state.get(entry['key']) or {}).get('trusted_hash') == entry['currentHash']:
            event = next(e for e, s in SNAKE.items() if entry['key'].endswith(':%s:0:0' % s))
            trusted[event] = HOOKS[event]
    RECORD['trusted'] = sorted(trusted)
    HOOKS = trusted


def connect():
    """Its MCP servers, as Codex starts them with a thread: their tools listed."""
    for key in CONFIG:
        if key.startswith('mcp_servers.'):
            name = key[len('mcp_servers.'):]
            mcp(name, 'initialize', {'protocolVersion': '2025-06-18'})
            tools = mcp(name, 'tools/list', {})['result']['tools']
            RECORD['listed'][name] = [t['name'] for t in tools]


def main():
    global THREAD, TURN, POLICY
    while True:
        message = INBOX.get()
        if message is None:
            break
        method, params, rid = message.get('method'), message.get('params') or {}, message.get('id')
        RECORD['requests'].append({'method': method, 'params': params})
        save()
        with LOCK:
            with open(os.path.join(HERE, 'requests.jsonl'), 'a') as f:
                f.write(json.dumps({'pid': os.getpid(), 'method': method, 'params': params}) + '\n')
        if rid is None:
            continue
        if method == 'initialize':
            result = {'userAgent': 'rovik/0.164.0', 'codexHome': os.environ.get('CODEX_HOME', '~/.codex'),
                      'platformFamily': 'unix', 'platformOs': 'linux'}
        elif method == 'account/login/start':
            result = {'type': params.get('type')}
        elif method == 'hooks/list':
            result = {'data': [{'cwd': os.getcwd(), 'hooks': listed_hooks(), 'warnings': [], 'errors': []}]}
        elif method in ('thread/start', 'thread/resume'):
            if method == 'thread/resume' and params.get('threadId') not in SCENARIO.get('threads', []):
                send({'id': rid, 'error': {'code': -32600,
                                           'message': 'no rollout found for thread id %s' % params.get('threadId')}})
                continue
            POLICY = params
            trust(params.get('config'))
            connect()
            THREAD = params.get('threadId') or SCENARIO.get('thread', 'th-1')
            result = {'thread': {'id': THREAD, 'cliVersion': '0.164.0', 'model': 'gpt-6.1-sol'}, 'model': 'gpt-6.1-sol'}
        elif method == 'skills/list':
            result = {'data': [{'cwd': os.getcwd(), 'skills': SCENARIO.get('skills', []), 'errors': []}]}
        elif method == 'model/list':
            result = {'data': SCENARIO.get('models', [])}
        elif method in ('turn/start', 'thread/compact/start', 'review/start'):
            TURN = 'tu-%d' % len(RECORD['requests'])
            send({'id': rid, 'result': {'turn': {'id': TURN, 'status': 'inProgress', 'items': []}}})
            note('turn/started', {'turn': {'id': TURN, 'status': 'inProgress'}})
            threading.Thread(target=play, args=(SCENARIO.get('steps', []),), daemon=True).start()
            continue
        elif method == 'turn/interrupt':
            result = {}
        else:
            send({'id': rid, 'error': {'code': -32601, 'message': 'unknown method ' + str(method)}})
            continue
        send({'id': rid, 'result': result})
    save()


main()
