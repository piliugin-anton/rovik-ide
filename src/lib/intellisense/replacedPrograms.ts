// replacedPrograms.ts
//
// TypeScript programs the worker's language service replaced, emptied
// (projectWorker.ts). Under JavaScriptCore, the web view of Linux and
// macOS, each program stays reachable from the next one: TypeScript's
// createProgram keeps its `arguments` alive in its closures, and its first
// argument, the options the language service builds, holds `oldProgram`.
// So every program built while typing (one per pause, each with its type
// checker) lived as long as the newest: in a big project the worker grew
// by hundreds of MB a minute of typing, until the web view was killed. V8
// (WebView2 on Windows) lets them go.
//
// The language service only ever uses its current program. Once replaced,
// a program is emptied: it then holds nothing (no methods, so no closures,
// no checker, no types), and the chain is cut at every link.

/** The language service, as far as this module needs it. */
interface Service {
  /** The current program, without bringing it up to date first. */
  getCurrentProgram(): object | undefined;
}

/**
 * The language service, with each program it replaces emptied once the
 * call that replaced it returns. `onNewProgram` is told of each new one.
 */
export function releasingReplacedPrograms<T extends Service>(
  service: T,
  onNewProgram: () => void = () => {}
): T {
  let current: object | undefined;
  const release = () => {
    const now = service.getCurrentProgram();
    if (now === current) return;
    if (current) emptyProgram(current);
    current = now;
    if (now) onNewProgram();
  };
  const methods = new Map<PropertyKey, (...args: unknown[]) => unknown>();
  return new Proxy(service, {
    get(target, key) {
      const value: unknown = Reflect.get(target, key);
      if (typeof value !== 'function' || key === 'getCurrentProgram') return value;
      let method = methods.get(key);
      if (!method) {
        method = (...args: unknown[]) => {
          try {
            return value.apply(target, args);
          } finally {
            release();
          }
        };
        methods.set(key, method);
      }
      return method;
    },
  });
}

/** A program's own properties (its methods) dropped, and with them what they hold. */
export function emptyProgram(program: object): void {
  for (const key of Object.keys(program)) {
    delete (program as Record<string, unknown>)[key];
  }
}
