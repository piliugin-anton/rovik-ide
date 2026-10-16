import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { emptyProgram, releasingReplacedPrograms } from './replacedPrograms';

/** A language service whose `edit` builds a new program, as an edit does. */
function fakeService() {
  const program = (n: number) => ({ n, getTypeChecker: () => ({ n }) });
  const service = {
    program: program(1),
    built: 1,
    getCurrentProgram() {
      return this.program;
    },
    getProgram() {
      return this.program;
    },
    edit(text: string) {
      this.program = program(++this.built);
      return `edited: ${text}`;
    },
    fail(): never {
      this.program = program(++this.built);
      throw new Error('worker trouble');
    },
  };
  return service;
}

describe('releasingReplacedPrograms', () => {
  it('empties a program once a call replaces it, and keeps the current one', () => {
    const service = releasingReplacedPrograms(fakeService());
    const first = service.getProgram();
    assert.equal(service.edit('a'), 'edited: a');
    const second = service.getProgram();
    assert.notEqual(second, first);
    assert.deepEqual(Object.keys(first), []);
    assert.equal(second.n, 2);
    assert.equal(second.getTypeChecker().n, 2);
    // A call that builds nothing new leaves the current program as it is.
    service.getProgram();
    assert.equal(second.n, 2);
    service.edit('b');
    assert.deepEqual(Object.keys(second), []);
    assert.equal(service.getProgram().n, 3);
  });

  it('a call that throws still lets the replaced program go', () => {
    const service = releasingReplacedPrograms(fakeService());
    const first = service.getProgram();
    assert.throws(() => service.fail(), /worker trouble/);
    assert.deepEqual(Object.keys(first), []);
    assert.equal(service.getProgram().n, 2);
  });

  it('reads that are not calls pass through', () => {
    const service = releasingReplacedPrograms(fakeService());
    assert.equal(service.built, 1);
    assert.equal(service.getCurrentProgram, service.getCurrentProgram);
  });
});

describe('emptyProgram', () => {
  it('drops every own property', () => {
    const program = { a: 1, b: () => 2 };
    emptyProgram(program);
    assert.deepEqual(Object.keys(program), []);
  });
});
