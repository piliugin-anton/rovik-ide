#lang racket
(define x 1)
(define (add a b) (+ a b))
(struct point (x y))
(define-syntax-rule (swap a b) (void))
