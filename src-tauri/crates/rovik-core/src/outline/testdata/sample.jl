module App
using LinearAlgebra
const MAX = 10
abstract type Shape end
struct Circle <: Shape
    r::Float64
end
function area(c::Circle)
    return pi * c.r^2
end
square(x) = x * x
macro m(x) x end
end
