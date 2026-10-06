module Main where
data Nat : Set where
  zero : Nat
  suc  : Nat -> Nat
_+_ : Nat -> Nat -> Nat
zero + n = n
suc m + n = suc (m + n)
record Point : Set where
  field x : Nat
