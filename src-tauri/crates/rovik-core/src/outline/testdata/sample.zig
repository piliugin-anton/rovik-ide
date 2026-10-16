const std = @import("std");
pub const Point = struct {
    x: i32,
    pub fn init(x: i32) Point { return .{ .x = x }; }
};
const Color = enum { red, green };
pub fn main() !void {
    const local = 1;
}
test "basic" {}
