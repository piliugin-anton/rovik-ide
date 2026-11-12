"""Versioned native-verifier ablation. No LLM quality or cost is inferred."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path('/build')
bank = json.loads(Path('/oracle/bank.json').read_text())
environment = json.loads(Path('/oracle/environment.json').read_text())
assert subprocess.check_output(['/usr/bin/gcc', '-dumpversion'], text=True).strip() == '12'
assert subprocess.check_output(['/usr/local/cargo/bin/rustc', '--version'], text=True).split()[1] == '1.85.1'
C = {
 'checked-add': ('int op(uint64_t a,uint64_t b,uint64_t *out) { if (UINT64_MAX-a < b) return 1; *out=a+b; return 0; }',
   ['uint64_t out=99; assert(op(2,3,&out)==0 && out==5);', 'assert(op(UINT64_MAX,0,&out)==0 && out==UINT64_MAX);', 'out=99; assert(op(UINT64_MAX,1,&out)==1 && out==99);', 'assert(op(0,0,&out)==0 && out==0);']),
 'decimal': ('int op(const char *s,uint64_t *out) { uint64_t value=0; if (!*s) return 1; for (;*s;++s) { if (*s<\'0\' || *s>\'9\') return 1; uint64_t digit=(uint64_t)(*s-\'0\'); if (value>(UINT64_MAX-digit)/10) return 1; value=value*10+digit; } *out=value; return 0; }',
   ['uint64_t out=99; assert(op("42",&out)==0 && out==42);', 'out=99; assert(op("4x",&out)==1 && out==99);', 'assert(op("",&out)==1 && out==99);', 'assert(op("18446744073709551616",&out)==1 && out==99);', 'assert(op("+1",&out)==1 && out==99);']),
 'copy': ('int op(unsigned char *out,size_t capacity,const unsigned char *source,size_t length) { if (length>capacity) return 1; memcpy(out,source,length); return 0; }',
   ['unsigned char out[4]={91,91,91,91}; const unsigned char source[4]={1,2,3,4}; assert(op(out,4,source,2)==0 && out[0]==1 && out[1]==2 && out[2]==91);', 'out[0]=91; assert(op(out,2,source,3)==1 && out[0]==91);', 'assert(op(out,0,source,0)==0 && out[0]==91);', 'assert(op(out,4,source,4)==0 && out[3]==4);'])
}
CPP = {
 'checked-add': ('std::optional<uint64_t> op(uint64_t a,uint64_t b) { if (UINT64_MAX-a < b) return {}; return a+b; }', ['assert(op(2,3)==5);','assert(op(UINT64_MAX,0)==UINT64_MAX);','assert(!op(UINT64_MAX,1));','assert(op(0,0)==0);']),
 'decimal': ('std::optional<uint64_t> op(std::string_view s) { uint64_t value=0; auto result=std::from_chars(s.data(),s.data()+s.size(),value); if (result.ec!=std::errc{} || result.ptr!=s.data()+s.size()) return {}; return value; }', ['assert(op("42")==42);','assert(!op("4x"));','assert(!op(""));','assert(!op("18446744073709551616"));','assert(!op("+1"));']),
 'copy': ('bool op(std::span<unsigned char> out,std::span<const unsigned char> source) { if (source.size()>out.size()) return false; std::copy(source.begin(),source.end(),out.begin()); return true; }', ['unsigned char out[4]={91,91,91,91}; const unsigned char source[4]={1,2,3,4}; assert(op(out,std::span(source).first(2)) && out[0]==1 && out[1]==2 && out[2]==91);','out[0]=91; assert(!op(std::span(out).first(2),std::span(source).first(3)) && out[0]==91);','assert(op(std::span(out).first(0),std::span(source).first(0)) && out[0]==91);','assert(op(out,source) && out[3]==4);'])
}
RUST = {
 'checked-add': ('fn op(a:u64,b:u64)->Option<u64> { if u64::MAX-a < b {None} else {Some(a+b)} }', ['assert_eq!(op(2,3),Some(5));','assert_eq!(op(u64::MAX,0),Some(u64::MAX));','assert_eq!(op(u64::MAX,1),None);','assert_eq!(op(0,0),Some(0));']),
 'decimal': ('fn op(input:&str)->Option<u64> { if input.is_empty() || input.bytes().any(|c|!c.is_ascii_digit()) {None} else {input.parse().ok()} }', ['assert_eq!(op("42"),Some(42));','assert_eq!(op("4x"),None);','assert_eq!(op(""),None);','assert_eq!(op("18446744073709551616"),None);','assert_eq!(op("+1"),None);']),
 'copy': ('fn op(out:&mut [u8],source:&[u8])->bool { if source.len()>out.len() {return false;} out[..source.len()].copy_from_slice(source); true }', ['let mut out=[91u8;4]; let source=[1u8,2,3,4]; assert!(op(&mut out,&source[..2]) && out==[1,2,91,91]);','out[0]=91; assert!(!op(&mut out[..2],&source[..3]) && out[0]==91);','assert!(op(&mut out[..0],&source[..0]) && out[0]==91);','assert!(op(&mut out,&source) && out==source);'])
}

def program(language, task, mutant):
    body, tests = {'c': C, 'cpp': CPP, 'rust': RUST}[language][task]
    if mutant:
        if task == 'checked-add':
            body = body.replace(' < b', ' <= b')
        elif task == 'decimal':
            if language == 'c':
                body = body.replace("if (*s<'0' || *s>'9') return 1;", "if (*s<'0' || *s>'9') { *out=value; return 0; }")
            elif language == 'cpp':
                body = body.replace(' || result.ptr!=s.data()+s.size()', '')
            else:
                body = body.replace('if input.is_empty()', "let input=input.trim_end_matches(|c:char|!c.is_ascii_digit()); if input.is_empty()")
        else:
            if language == 'c':
                body = body.replace('if (length>capacity) return 1;', 'if (length>capacity) { if(capacity) out[0]=0; return 1; }')
            elif language == 'cpp':
                body = body.replace('if (source.size()>out.size()) return false;', 'if (source.size()>out.size()) { if(!out.empty()) out[0]=0; return false; }')
            else:
                body = body.replace('{return false;}', '{if !out.is_empty() {out[0]=0;} return false;}')
    if language == 'rust':
        lines = [body, 'fn main() { let full=std::env::args().nth(1).as_deref()==Some("full");']
        for number, test in enumerate(tests):
            if number == 1:
                lines.append('if !full {return;}')
            lines += [f'println!("case:{number+1}");', test]
        lines.append('}')
    else:
        lines = ['#include <assert.h>\n#include <stdint.h>\n#include <stdio.h>\n#include <string.h>']
        if language == 'cpp':
            lines += ['#include <algorithm>\n#include <charconv>\n#include <optional>\n#include <span>\n#include <string_view>']
        lines += [body, 'int main(int argc,char **argv) { int full=argc>1 && !strcmp(argv[1],"full");']
        for number, test in enumerate(tests):
            if number == 1:
                lines.append('if (!full) return 0;')
            lines += [f'printf("case:{number+1}\\n"); fflush(stdout);', test]
        lines.append('return 0; }')
    return '\n'.join(lines), len(tests)

observations = []
for case in bank['cases']:
    language, variant = case['language'], case['case_id'].rsplit('-', 1)[1]
    task = case['case_id'][len(language)+1:-(len(variant)+1)]
    source, count = program(language, task, variant == 'mutant')
    assert count == case['required_cases']
    for repetition in range(bank['repetitions']):
        directory = ROOT / f"{case['case_id']}-{repetition}"
        directory.mkdir()
        path = directory / ('main.' + {'c': 'c', 'cpp': 'cpp', 'rust': 'rs'}[language])
        path.write_text(source)
        binary = directory / 'consumer'
        if language == 'rust':
            command = ['/usr/local/cargo/bin/rustc', '--edition=2021', '--target=x86_64-unknown-linux-gnu', '-Dwarnings', '-Funsafe-code', str(path), '-o', str(binary)]
        else:
            command = ['/usr/bin/gcc' if language == 'c' else '/usr/bin/g++', '-std=c17' if language == 'c' else '-std=c++23', '-Wall', '-Wextra', '-Werror', str(path), '-o', str(binary)]
        start = time.monotonic_ns()
        compiled = subprocess.run(command, stdout=sys.stderr, stderr=sys.stderr, timeout=60)
        compile_ms = (time.monotonic_ns()-start)//1000000
        for strategy in ['compile_only', 'example_only', 'protected_contract']:
            duration, executed, outcome = compile_ms, 0, 'error'
            if compiled.returncode == 0:
                if strategy == 'compile_only':
                    outcome = 'accepted'
                else:
                    start = time.monotonic_ns()
                    result = subprocess.run([str(binary), 'full' if strategy == 'protected_contract' else 'example'], stdout=subprocess.PIPE, stderr=sys.stderr, timeout=10)
                    duration += (time.monotonic_ns()-start)//1000000
                    expected = count if strategy == 'protected_contract' else 1
                    lines = result.stdout.decode().splitlines()
                    valid = lines == [f'case:{i+1}' for i in range(len(lines))] and 0 < len(lines) <= expected
                    executed = len(lines) if valid else 0
                    if valid:
                        outcome = 'accepted' if result.returncode == 0 and executed == expected else ('rejected' if result.returncode != 0 else 'error')
            observations.append(dict(case_id=case['case_id'], repetition=repetition, strategy=strategy, outcome=outcome, executed_cases=executed, duration_ms=duration))
canonical = json.dumps(bank, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()
report = dict(schema_version=1, bank_digest='sha256:'+hashlib.sha256(canonical).hexdigest(), environment_digest=environment['digest'], methodology='native-verifier-ablation; no model calls', observations=observations)
Path('/outputs/evaluation.json').write_text(json.dumps(report))
