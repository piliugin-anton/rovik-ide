# Offline dependency fixtures

These archives contain project-owned identity functions, with no external code,
build hooks or native extensions. They exercise actual offline npm and pip
installation from the immutable dependency mount. They are test inputs, not
harness controllers.

- `fixture-identity-1.0.0.tgz`: npm package `fixture-identity` 1.0.0, ESM;
  `index.mjs` contains `export default function identity(value) { return value; }`.
- `fixture_identity-1.0.0-py3-none-any.whl`: Python wheel `fixture-identity`
  1.0.0; `fixture_identity/__init__.py` contains `def identity(value): return value`.

The fixture locks bind the complete archives with npm integrity or a pip SHA-256
hash. Missing or corrupted archives must block verification. Inspect their full
contents with `tar -tzf` and `unzip -l`, respectively.
