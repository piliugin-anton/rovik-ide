import assert from 'node:assert/strict';
import test from 'node:test';
import { verifyLock } from './verify-lock.mjs';

const trusted = [
  ['rovik-harness', '1.0.0', null, null],
  ['dependency', '2.0.0', 'registry+https://github.com/rust-lang/crates.io-index', 'abc'],
  ['unused-workspace-feature', '3.0.0', 'registry+https://github.com/rust-lang/crates.io-index', 'xyz'],
];
const lock = `version = 4
[[package]]
name = "rovik-harness"
version = "1.0.0"
dependencies = ["dependency"]
[[package]]
name = "dependency"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"
`;

test('standalone feature normalization may remove unused workspace packages', () => {
  verifyLock(lock, trusted);
});

test('normalization cannot upgrade versions, replace checksums or change sources', () => {
  for (const changed of [
    lock.replace('"2.0.0"', '"2.1.0"'),
    lock.replace('"abc"', '"different"'),
    lock.replace('registry+https://github.com/rust-lang/crates.io-index', 'registry+https://untrusted.invalid'),
    lock + '\n[[package]]\nname = "unexpected"\nversion = "1.0.0"\n',
  ]) assert.throws(() => verifyLock(changed, trusted), /differs from the shipped lock/);
});

test('incomplete or ambiguous locks cannot authorize compilation', () => {
  assert.throws(() => verifyLock('', trusted), /no packages/);
  assert.throws(() => verifyLock(lock.replace('name = "rovik-harness"', 'name = "dependency"'), trusted));
  assert.throws(() => verifyLock(lock.replace('checksum = "abc"', 'checksum = "abc"\nchecksum = "abc"'), trusted), /Invalid verifier lock field/);
});
