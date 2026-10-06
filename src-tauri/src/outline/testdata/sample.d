module app.main;
import std.stdio;
struct Point { int x; }
class Shape { this() {} ~this() {} abstract double area(); }
interface Drawable { void draw(); }
enum Color { red, green }
void main() { writeln("x"); }
unittest { assert(true); }
alias Name = string;
