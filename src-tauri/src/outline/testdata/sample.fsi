module App
type Shape =
    | Circle of float
    | Square of float
val area : Shape -> float
exception MyError of string
