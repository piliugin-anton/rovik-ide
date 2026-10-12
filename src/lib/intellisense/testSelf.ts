// Test support: Monaco's worker module assigns `self.onmessage` when it
// loads, which only exists in browsers. Imported first by a test that
// loads the worker under Node.
(globalThis as { self?: unknown }).self ??= globalThis;
