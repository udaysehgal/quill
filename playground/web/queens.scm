;; Count the solutions to the N-queens puzzle.

(define (safe? col placed)
  ;; `placed` holds the columns of the queens in the rows above, nearest first
  (let loop ([placed placed] [distance 1])
    (or (null? placed)
        (let ([c (car placed)])
          (and (not (= c col))
               (not (= (abs (- c col)) distance))
               (loop (cdr placed) (add1 distance)))))))

(define (queens n)
  (let place ([row 0] [placed '()])
    (if (= row n)
        1
        (fold-left + 0
                   (map (lambda (col)
                          (if (safe? col placed) (place (add1 row) (cons col placed)) 0))
                        (iota n))))))

(for-each (lambda (n)
            (display n) (display " queens: ") (display (queens n)) (newline))
          (iota 9))
