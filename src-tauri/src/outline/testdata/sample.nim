import strutils
type Point = object
  x: int
proc add(a, b: int): int =
  a + b
func pure(x: int): int = x
