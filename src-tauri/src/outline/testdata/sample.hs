module Main where
import Data.List
data Shape = Circle Double | Square Double
newtype Wrapper = Wrapper Int
type Name = String
class Describe a where
  describe :: a -> String
instance Describe Shape where
  describe _ = "shape"
area :: Shape -> Double
area (Circle r) = pi * r * r
area (Square s) = s * s
main :: IO ()
main = print 1
