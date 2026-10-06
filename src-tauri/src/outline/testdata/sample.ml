open Printf
type shape = Circle of float | Square of float
let area = function Circle r -> r | Square s -> s
let rec fact n = if n = 0 then 1 else n * fact (n - 1)
module Stack = struct
  type t = int list
  let empty = []
end
module type S = sig val x : int end
exception Not_found_here
class point = object method x = 0 end
