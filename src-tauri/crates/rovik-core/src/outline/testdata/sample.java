package a;
import java.util.*;
@Annotated
public class Outer<T> extends Base implements I {
    private int field;
    public Outer() {}
    @Override
    public String toString() { return ""; }
    static class Inner {}
    interface Nested { void m(); }
    enum E { A, B; void m() {} }
}
record Point(int x, int y) { Point { } }
@interface Ann { String value(); }
