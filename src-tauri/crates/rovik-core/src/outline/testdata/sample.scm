(define x 1)
(define (add a b) (+ a b))
(define-record-type point (make-point x y) point? (x point-x))
(define-syntax my-if (syntax-rules () ((_ c a b) (cond (c a) (else b)))))
