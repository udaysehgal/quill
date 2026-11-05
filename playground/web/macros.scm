;; syntax-rules macros, quasiquote and rest arguments.

(define-syntax while
  (syntax-rules ()
    [(_ condition body ...)
     (let loop () (when condition body ... (loop)))]))

(define-syntax swap!
  (syntax-rules ()
    [(_ a b) (let ([tmp a]) (set! a b) (set! b tmp))]))

(define (collatz-steps n)
  (let ([steps 0])
    (while (> n 1)
      (set! n (if (even? n) (quotient n 2) (+ (* 3 n) 1)))
      (set! steps (add1 steps)))
    steps))

(define (describe name . values)
  `(,name has ,(length values) values: ,@values))

(let ([tmp 'first] [other 'second])
  (swap! tmp other)          ; hygienic: the macro's own tmp doesn't clash
  (display (list tmp other))
  (newline))

(display (describe 'collatz (collatz-steps 27) (collatz-steps 97)))
(newline)
