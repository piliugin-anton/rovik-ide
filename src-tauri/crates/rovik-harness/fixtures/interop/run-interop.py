"""Protected regression bank: actual C producer and C/C++/Rust consumers."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ORACLE = Path('/oracle')
BUILD = Path('/build')
source = (ORACLE / 'packet.c').read_text()
variants = {
    'correct': source,
    'partial-leak': source.replace('if (!value->second) { free(value->first);', 'if (!value->second) {'),
    'failure-output': source.replace('packet *value = malloc', '*out = NULL; packet *value = malloc'),
    'abi-value': source.replace('out->size = value->size;', 'out->size = value->size + 1;'),
    'undefined-overflow': source.replace('packet *value = malloc', 'volatile int32_t limit = INT32_MAX; limit += (int32_t)size; packet *value = malloc'),
    'buffer-overrun': source.replace('i < size;', 'i <= size;'),
    'retained-callback': source.replace('struct packet {', 'static packet_callback retained; static void *retained_context;\nstruct packet {')
        .replace('if (callback) callback(context, checksum);', 'retained = callback; retained_context = context; if (callback) callback(context, checksum);')
        .replace('free(value->second);', 'if (retained) { retained(retained_context, 1); } free(value->second);'),
}
flags = ['-g', '-O1', '-fno-omit-frame-pointer', '-Wall', '-Wextra', '-Werror']
san = ['-fsanitize=address,undefined', '-fno-sanitize-recover=all']
# Both native sanitizers and the controlled allocator check resource failures.
environment = dict(os.environ, ASAN_OPTIONS='detect_leaks=1:abort_on_error=1', UBSAN_OPTIONS='halt_on_error=1')
results = []
def run(argv):
    subprocess.run(argv, check=True, timeout=90, stdout=sys.stderr, stderr=sys.stderr)
def instrumented(path):
    symbols = subprocess.check_output(['/usr/bin/nm', '-u', str(path)], text=True)
    return '__asan_' in symbols and '__ubsan_' in symbols
for name, text in variants.items():
    directory = BUILD / name
    directory.mkdir()
    (directory / 'packet.c').write_text(text)
    # Rust ABI execution is separate from the GCC address/undefined sanitizer modes.
    languages = ['c', 'cpp'] if name in ('buffer-overrun', 'undefined-overflow') else ['c', 'cpp', 'rust']
    for language in languages:
        start = time.monotonic_ns()
        extra = [] if language == 'rust' else san
        producer = directory / (language + '-producer.o')
        allocator = directory / (language + '-allocator.o')
        run(['/usr/bin/gcc', '-std=c17', *flags, *extra, '-I/oracle', '-Dmalloc=harness_malloc', '-Dfree=harness_free', '-c', str(directory / 'packet.c'), '-o', str(producer)])
        run(['/usr/bin/gcc', '-std=c17', *flags, *extra, '-c', '/oracle/allocator.c', '-o', str(allocator)])
        if language != 'rust':
            assert instrumented(producer), 'required sanitizer instrumentation is absent'
        executable = directory / language
        if language == 'rust':
            archive = directory / 'libpacket.a'
            run(['/usr/bin/ar', 'rcs', str(archive), str(producer), str(allocator)])
            run(['/usr/local/cargo/bin/rustc', '--edition=2021', '-Dwarnings', '/oracle/consumer.rs', '-L', str(directory), '-lstatic=packet', '-o', str(executable)])
        else:
            compiler = '/usr/bin/gcc' if language == 'c' else '/usr/bin/g++'
            run([compiler, '-std=c17' if language == 'c' else '-std=c++23', *flags, *extra, '-I/oracle', '/oracle/consumer.c', str(producer), str(allocator), '-o', str(executable)])
        observed = subprocess.run([str(executable)], env=environment, timeout=15, stdout=sys.stderr, stderr=sys.stderr)
        expected = observed.returncode == 0 if name == 'correct' else observed.returncode != 0
        results.append(dict(id=language + '-' + name, passed=expected, exit_code=observed.returncode, duration_ms=(time.monotonic_ns()-start)//1000000))
plain = BUILD / 'uninstrumented.o'
run(['/usr/bin/gcc', '-std=c17', '-I/oracle', '-c', '/oracle/packet.c', '-o', str(plain)])
results.append(dict(id='removed-sanitizers-rejected', passed=not instrumented(plain), exit_code=0, duration_ms=0))
Path('/outputs/observations.json').write_text(json.dumps(results))
cases = [r['id'] for r in results]
Path('/outputs/cases.json').write_text(json.dumps(dict(schema_version=1,discovered=cases,selected=cases,passed=[r['id'] for r in results if r['passed']],failed=[r['id'] for r in results if not r['passed']],skipped=[])))
sys.exit(any(not r['passed'] for r in results))
