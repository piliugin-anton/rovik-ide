"""Protected C/C++ driver. Configuration is immutable oracle input."""
import json
from pathlib import Path
import subprocess
import sys

profile = json.loads(Path('/oracle/profile.json').read_text())
config = profile['configurations'][0]
cpp = profile['language'] == 'cpp'
compiler = '/usr/bin/g++' if cpp else '/usr/bin/gcc'
def run(args):
    subprocess.run(args, check=True, timeout=90)
assert subprocess.check_output([compiler, '-dumpmachine'], text=True).strip() == config['target']
assert subprocess.check_output([compiler, '-dumpversion'], text=True).strip() == '12'
flags = config['flags']
run([compiler, *flags, '-std=c++23' if cpp else '-std=c17', '-c', '/oracle/consumer.cpp' if cpp else '/oracle/consumer.c', '-o', '/build/consumer.o'])
objects = ['/build/consumer.o']
if not cpp:
    run([compiler, *flags, '-std=c17', '-c', '/snapshot/implementations/quota/quota.c', '-o', '/build/quota.o'])
    objects.append('/build/quota.o')
run([compiler, *flags, *objects, '-o', '/build/consumer'])
cases = json.loads(Path('/oracle/cases.json').read_text())
passed, failed = [], []
for case in cases:
    result = subprocess.run(['/build/consumer', case], timeout=10)
    (passed if result.returncode == 0 else failed).append(case)
Path('/outputs/cases.json').write_text(json.dumps(dict(schema_version=1, discovered=cases, selected=cases, passed=passed, failed=failed, skipped=[])))
sys.exit(bool(failed))
