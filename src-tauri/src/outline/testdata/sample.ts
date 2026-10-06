import { a, type B } from './x';
export default function App(props: P): JSX.Element {
  const inner = () => 1;
  return null;
}
export const routes: Route[] = [];
export class Foo<T> extends Bar implements Baz {
  private x = 1;
  handle = (e: E) => {};
  constructor(a: number) { super(); }
  abstract m(): void;
}
export interface Props { a: string; m(): void }
export type Alias = { a: string };
enum Color { Red }
declare module 'x' { export function f(): void; }
namespace NS { export const y = 1; function z() {} }
export abstract class Abs { abstract m(): void; }
function overload(a: string): void;
function overload(a: any) {}
export default class {}
