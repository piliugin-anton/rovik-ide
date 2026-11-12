#!/usr/bin/env python3
"""One bounded protocol-v1 invocation inside the registered verification sandbox."""
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys

LIMIT = 8 * 1024 * 1024
ROOT = Path('/snapshot')
BUILD = Path('/build/native')


class PackError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def require(condition, message, code='INVALID_CONFIGURATION'):
    if not condition:
        raise PackError(code, message)


def pairs(items):
    out = {}
    for key, value in items:
        require(key not in out, 'duplicate JSON key', 'PROTOCOL_ERROR')
        out[key] = value
    return out


def decode(data):
    require(len(data) <= LIMIT, 'input exceeds protocol bound', 'PROTOCOL_ERROR')
    return json.loads(data, object_pairs_hook=pairs,
                      parse_float=lambda _: (_ for _ in ()).throw(PackError('PROTOCOL_ERROR', 'floating point is unsupported')),
                      parse_constant=lambda _: (_ for _ in ()).throw(PackError('PROTOCOL_ERROR', 'non-finite number')))


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode('utf-8')


def digest(value):
    return 'sha256:' + hashlib.sha256(canonical(value)).hexdigest()


def run(argv, cwd=ROOT):
    try:
        result = subprocess.run(argv, cwd=cwd, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=120, check=False)
    except FileNotFoundError:
        raise PackError('MISSING_TOOL', 'registered tool missing: ' + argv[0])
    except subprocess.TimeoutExpired:
        raise PackError('TIMEOUT', 'native tool exceeded its deadline')
    require(len(result.stdout) + len(result.stderr) <= LIMIT,
            'native output exceeds limit', 'OUTPUT_LIMIT')
    if result.stderr:
        sys.stderr.buffer.write(result.stderr)
    require(result.returncode == 0, 'native command failed: ' + shlex.join(argv), 'NATIVE_FAILURE')
    return result.stdout.decode('utf-8')


def relative(value):
    path = Path(value)
    if not path.is_absolute():
        path = ROOT / path
    path = path.resolve()
    for root, prefix in [(ROOT, ''), (Path('/oracle'), 'oracle:'), (Path('/build'), 'generated:'),
                         (Path('/dependencies'), 'dependency:')]:
        try:
            return prefix + path.relative_to(root).as_posix()
        except ValueError:
            pass
    raise PackError('UNRESOLVED_GRAPH', 'graph references an undeclared source root: ' + str(path))


def configuration(payload):
    require(set(payload) == {'snapshot_handle', 'profile', 'configuration_id', 'forbidden_edges', 'required_test_ids'},
            'unexpected pack payload fields', 'PROTOCOL_ERROR')
    require(payload['snapshot_handle'] == 'snapshot:active', 'unknown snapshot handle')
    profile = payload['profile']
    require(profile['schema_version'] == 1 and profile['status'] == 'resolved', 'unresolved profile')
    configs = [c for c in profile['configurations'] if c['id'] == payload['configuration_id']]
    require(len(configs) == 1, 'unknown or ambiguous configuration')
    config = configs[0]
    require(config['runtime'] == 'linux-native', 'unsupported native runtime', 'UNSUPPORTED_CONFIGURATION')
    language = profile['language']
    if language == 'rust':
        version = run(['/usr/local/cargo/bin/rustc', '--version']).strip()
        require(version.split()[1] == profile['facts']['minimum_supported_rust_version'],
                'MSRV requires its own registered compiler configuration', 'UNSUPPORTED_CONFIGURATION')
        targets = run(['/usr/local/cargo/bin/rustup', 'target', 'list', '--installed']).splitlines()
        require(config['target'] in targets, 'target standard library is not installed', 'UNSUPPORTED_CONFIGURATION')
        require(profile['facts']['std_or_no_std'] == 'std', 'no_std needs a registered target runner', 'UNSUPPORTED_CONFIGURATION')
    elif language in ('c', 'cpp'):
        compiler = '/usr/bin/gcc' if language == 'c' else '/usr/bin/g++'
        version = run([compiler, '--version']).splitlines()[0]
        require(run([compiler, '-dumpversion']).strip() == '12', 'unregistered GCC major version', 'UNSUPPORTED_CONFIGURATION')
        require(config['target'] == run([compiler, '-dumpmachine']).strip(), 'unregistered compiler target', 'UNSUPPORTED_CONFIGURATION')
        require(profile['facts']['language_standard'] in (['17'] if language == 'c' else ['23']),
                'unregistered native language standard', 'UNSUPPORTED_CONFIGURATION')
    else:
        raise PackError('UNSUPPORTED_CONFIGURATION', 'unregistered language')
    return profile, config, version


def rust_graph(profile, config):
    # Only the pinned dependency bundle may seed the fresh offline Cargo cache.
    if Path('/dependencies/cargo').is_dir():
        shutil.copytree('/dependencies/cargo', '/build/cargo', dirs_exist_ok=True)
    args = ['/usr/local/cargo/bin/cargo', 'metadata', '--format-version=1', '--locked', '--offline',
            '--filter-platform', config['target']]
    if not config['default_features']:
        args.append('--no-default-features')
    if config['features']:
        args += ['--features', ','.join(config['features'])]
    metadata = decode(run(args).encode())
    require(metadata.get('resolve') is not None and 'nodes' in metadata['resolve'],
            'metadata without the complete resolve graph cannot establish dependencies', 'UNRESOLVED_GRAPH')
    packages = {p['id']: p for p in metadata['packages']}
    labels = {identity: p['name'] + '@' + p['version'] for identity, p in packages.items()}
    require(len(set(labels.values())) == len(labels), 'ambiguous package versions need explicit component mapping', 'UNRESOLVED_GRAPH')
    nodes = []
    for identity, package in packages.items():
        local = package.get('source') is None
        nodes.append({'id': labels[identity], 'path': relative(package['manifest_path']) if local else None,
                      'kind': 'package'})
    edges = []
    for node in metadata['resolve']['nodes']:
        require(node['id'] in packages, 'resolve graph references an unknown package', 'UNRESOLVED_GRAPH')
        for dependency in node['deps']:
            require(dependency['pkg'] in packages, 'resolve graph omitted a dependency', 'UNRESOLVED_GRAPH')
            for kind in dependency['dep_kinds']:
                edges.append({'source': labels[node['id']], 'target': labels[dependency['pkg']],
                              'kind': kind['kind'] or 'normal'})
    return {'precision': 'cargo_resolved', 'nodes': nodes, 'edges': edges, 'unresolved': [],
            'configuration_digest': digest(config), 'compiler_commands': [], 'test_inventory': []}


def cmake_graph(profile, config):
    require(not BUILD.exists(), 'build database must be collected in a fresh directory', 'STALE_BUILD_DATABASE')
    query = BUILD / '.cmake/api/v1/query/codemodel-v2'
    query.parent.mkdir(parents=True)
    query.write_text('')
    flags = shlex.join(config['flags'])
    argv = ['/usr/bin/cmake', '-S', str(ROOT), '-B', str(BUILD), '-G', 'Ninja',
            '-DCMAKE_BUILD_TYPE=Debug', '-DCMAKE_EXPORT_COMPILE_COMMANDS=ON',
            '-DCMAKE_C_COMPILER=/usr/bin/gcc', '-DCMAKE_CXX_COMPILER=/usr/bin/g++',
            '-DCMAKE_C_STANDARD=17', '-DCMAKE_CXX_STANDARD=23',
            '-DCMAKE_C_STANDARD_REQUIRED=ON', '-DCMAKE_CXX_STANDARD_REQUIRED=ON',
            '-DCMAKE_C_EXTENSIONS=OFF', '-DCMAKE_CXX_EXTENSIONS=OFF']
    argv.append(('-DCMAKE_C_FLAGS=' if profile['language'] == 'c' else '-DCMAKE_CXX_FLAGS=') + flags)
    for definition in config['features']:
        require(re.fullmatch(r'[A-Za-z_][A-Za-z_0-9]*=(ON|OFF|[0-9]+)', definition), 'unsupported CMake feature selection')
        argv.append('-D' + definition)
    run(argv)
    run(['/usr/bin/cmake', '--build', str(BUILD), '--parallel', '2', '--target', *config['build_targets']])
    database = decode((BUILD / 'compile_commands.json').read_bytes())
    require(isinstance(database, list) and database, 'empty compilation database', 'UNRESOLVED_GRAPH')
    nodes, edges, commands = {}, [], []
    for unit in database:
        args = unit.get('arguments') or shlex.split(unit['command'])
        require(args[0] in ('/usr/bin/gcc', '/usr/bin/g++', '/usr/bin/cc', '/usr/bin/c++'), 'unregistered compiler in build database')
        require(Path(unit['directory']).resolve().is_relative_to(BUILD), 'stale compilation directory', 'STALE_BUILD_DATABASE')
        source = relative(unit['file'])
        nodes[source] = {'id': source, 'path': source, 'kind': 'translation_unit'}
        commands.append({'file': source, 'argv': args})
        preprocess, skip = [], False
        for arg in args:
            if skip:
                skip = False
                continue
            if arg in ('-o', '-MF', '-MT', '-MQ'):
                skip = True
            elif arg not in ('-c', '-MD', '-MMD', '-MP'):
                preprocess.append(arg)
        output = run(preprocess + ['-MM', '-MT', '__harness'], Path(unit['directory']))
        require(output.startswith('__harness:'), 'malformed compiler dependency output', 'UNRESOLVED_GRAPH')
        for included in shlex.split(output.partition(':')[2].replace('\\\n', ' ')):
            target = relative(included)
            nodes.setdefault(target, {'id': target, 'path': target, 'kind': 'header'})
            if source != target:
                edges.append({'source': source, 'target': target, 'kind': 'include'})
    replies = BUILD / '.cmake/api/v1/reply'
    indices = list(replies.glob('index-*.json'))
    require(len(indices) == 1, 'missing or ambiguous CMake reply', 'UNRESOLVED_GRAPH')
    index = decode(indices[0].read_bytes())
    model = decode((replies / index['reply']['codemodel-v2']['jsonFile']).read_bytes())
    require(len(model['configurations']) == 1, 'ambiguous CMake configuration', 'UNRESOLVED_GRAPH')
    targets = {}
    for target in model['configurations'][0]['targets']:
        targets[target['id']] = target['name']
        name = 'target:' + target['name']
        nodes[name] = {'id': name, 'path': None, 'kind': 'build_target'}
    for target in model['configurations'][0]['targets']:
        detail = decode((replies / target['jsonFile']).read_bytes())
        for source in detail.get('sources', []):
            if 'compileGroupIndex' in source:
                unit = relative(source['path'])
                require(unit in nodes, 'codemodel source is absent from compiler database', 'UNRESOLVED_GRAPH')
                edges.append({'source': 'target:' + target['name'], 'target': unit, 'kind': 'compiles'})
        for dependency in detail.get('dependencies', []):
            require(dependency['id'] in targets, 'unresolved CMake target', 'UNRESOLVED_GRAPH')
            edges.append({'source': 'target:' + target['name'], 'target': 'target:' + targets[dependency['id']], 'kind': 'link_or_build'})
    tests = decode(run(['/usr/bin/ctest', '--test-dir', str(BUILD), '--show-only=json-v1']).encode())
    return {'precision': 'configured_build', 'nodes': list(nodes.values()), 'edges': edges, 'unresolved': [],
            'configuration_digest': digest(config), 'compiler_commands': commands,
            'test_inventory': [t['name'] for t in tests['tests']]}


def dispatch(operation, payload):
    if operation == 'explain':
        require(set(payload) == {'parser_version', 'diagnostic'} and payload['parser_version'] == 1,
                'unsupported diagnostic parser', 'PROTOCOL_ERROR')
        require(isinstance(payload['diagnostic'], str) and 0 < len(payload['diagnostic']) <= 65536,
                'invalid native diagnostic', 'PROTOCOL_ERROR')
        return {'diagnostics': [{'rule_id': 'native-log', 'message': payload['diagnostic']}]}
    if operation == 'plan_checks':
        require(set(payload) == {'plan', 'registered_commands'}, 'invalid plan payload', 'PROTOCOL_ERROR')
        require(all(c['command_id'] in payload['registered_commands'] for c in payload['plan']['checks']), 'unregistered plan command')
        return {'plan': payload['plan']}
    require(operation in ('probe', 'index'), 'unknown operation', 'PROTOCOL_ERROR')
    profile, config, compiler = configuration(payload)
    if operation == 'probe':
        return {'language': profile['language'], 'profile_resolution': 'resolved', 'compiler': compiler,
                'configuration_digest': digest(config), 'observed': {'compiler': compiler},
                'configured': profile['facts'], 'uncertain': [],
                'capabilities': ['compile', 'protected-consumer', 'cargo-resolved-graph' if profile['language'] == 'rust' else 'configured-build-graph'],
                'unsupported_obligations': []}
    graph = rust_graph(profile, config) if profile['language'] == 'rust' else cmake_graph(profile, config)
    return {'language': profile['language'], 'graph': graph}


def main():
    request_id = 'invalid-request'
    try:
        data = sys.stdin.buffer.read(LIMIT + 1)
        require(data.endswith(b'\n'), 'request must end with newline', 'PROTOCOL_ERROR')
        request = decode(data)
        require(set(request) == {'protocol_version', 'request_id', 'operation', 'payload'}, 'unexpected envelope field', 'PROTOCOL_ERROR')
        require(request['protocol_version'] == 1, 'unsupported protocol version', 'PROTOCOL_ERROR')
        request_id = request['request_id']
        require(isinstance(request_id, str) and 0 < len(request_id) <= 160, 'invalid request ID', 'PROTOCOL_ERROR')
        result = dispatch(request['operation'], request['payload'])
        response = {'protocol_version': 1, 'request_id': request_id, 'ok': True, 'result': result}
        code = 0
    except (PackError, ValueError, KeyError, TypeError, OSError) as error:
        response = {'protocol_version': 1, 'request_id': request_id, 'ok': False,
                    'error': {'code': getattr(error, 'code', 'INVALID_NATIVE_REPORT'), 'message': str(error), 'retryable': False}}
        code = 2
    output = canonical(response) + b'\n'
    if len(output) > LIMIT:
        raise SystemExit(2)
    sys.stdout.buffer.write(output)
    raise SystemExit(code)


if __name__ == '__main__':
    main()
