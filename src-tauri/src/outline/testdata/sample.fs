module App
open System
type Shape = Circle of float | Square of float
let area shape = match shape with Circle r -> r | Square s -> s
let rec fact n = if n = 0 then 1 else n * fact (n - 1)
type Point(x: int) =
    member this.X = x
exception MyError of string
