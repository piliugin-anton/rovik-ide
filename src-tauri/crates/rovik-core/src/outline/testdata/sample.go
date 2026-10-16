package main

import "fmt"

type (
	A struct{ x int }
	B int
)

type Shape interface {
	Area() float64
}

func (s *Square) Area() float64 { return 0 }

func main() {
	x := 1
	fmt.Println(x)
}

const (
	X = 1
	Y = 2
)

var z = 3

type Alias = int
