;; Strings, characters, symbols and sorting: count word frequencies.

(define text "the quick brown fox jumps over the lazy dog the fox")

(define (split-words s)
  (let loop ([chars (string->list s)] [word '()] [words '()])
    (define (flush) (if (null? word) words (cons (list->string (reverse word)) words)))
    (cond [(null? chars) (reverse (flush))]
          [(char=? (car chars) #\space) (loop (cdr chars) '() (flush))]
          [else (loop (cdr chars) (cons (car chars) word) words)])))

(define (count-words words)
  (let loop ([words words] [counts '()])
    (if (null? words)
        counts
        (let* ([w (string->symbol (car words))]
               [entry (assq w counts)])
          (if entry
              (begin (set-cdr! entry (add1 (cdr entry))) (loop (cdr words) counts))
              (loop (cdr words) (cons (cons w 1) counts)))))))

(for-each (lambda (entry)
            (display (car entry)) (display ": ") (display (cdr entry)) (newline))
          (sort (count-words (split-words text))
                (lambda (a b) (> (cdr a) (cdr b)))))
