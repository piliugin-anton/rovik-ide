import 'dart:io';
const int maxSize = 10;
int topLevel(int x) {
  return x;
}
String get name => 'x';
class Point extends Base with M implements I {
  final int x;
  Point(this.x);
  factory Point.zero() => Point(0);
  int distance(Point other) {
    return 0;
  }
  static void helper() {}
}
mixin M on Base {}
enum Color { red, green }
extension Ext on String { int twice() => 2; }
typedef Fn = int Function(int);
