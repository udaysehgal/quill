;;; schemec standard library, written in the subset schemec itself compiles.
;;; It is prepended to every program, but only the definitions a program
;;; (transitively) uses are compiled. A user definition with the same name wins.
;;; Names starting with % are internal helpers.

;; ───────── lists ─────────

(define (length l)
  (let loop ([l l] [n 0])
    (if (null? l) n (loop (cdr l) (add1 n)))))

(define (append a b)
  (if (null? a) b (cons (car a) (append (cdr a) b))))

(define (reverse l)
  (let loop ([l l] [acc '()])
    (if (null? l) acc (loop (cdr l) (cons (car l) acc)))))

(define (list-tail l k)
  (if (zero? k) l (list-tail (cdr l) (sub1 k))))

(define (list-ref l k) (car (list-tail l k)))

(define (map f l)
  (if (null? l) '() (cons (f (car l)) (map f (cdr l)))))

(define (for-each f l)
  (unless (null? l)
    (f (car l))
    (for-each f (cdr l))))

(define (filter keep? l)
  (cond [(null? l) '()]
        [(keep? (car l)) (cons (car l) (filter keep? (cdr l)))]
        [else (filter keep? (cdr l))]))

(define (fold-left f acc l)
  (if (null? l) acc (fold-left f (f acc (car l)) (cdr l))))

(define (fold-right f init l)
  (if (null? l) init (f (car l) (fold-right f init (cdr l)))))

(define (memq x l)
  (cond [(null? l) #f] [(eq? x (car l)) l] [else (memq x (cdr l))]))

(define (member x l)
  (cond [(null? l) #f] [(equal? x (car l)) l] [else (member x (cdr l))]))

(define (assq x l)
  (cond [(null? l) #f] [(eq? x (car (car l))) (car l)] [else (assq x (cdr l))]))

(define (assoc x l)
  (cond [(null? l) #f] [(equal? x (car (car l))) (car l)] [else (assoc x (cdr l))]))

(define (equal? a b)
  (cond [(eq? a b) #t]
        [(and (pair? a) (pair? b))
         (and (equal? (car a) (car b)) (equal? (cdr a) (cdr b)))]
        [(and (string? a) (string? b)) (string=? a b)]
        [(and (vector? a) (vector? b))
         (and (= (vector-length a) (vector-length b))
              (let loop ([i 0])
                (or (= i (vector-length a))
                    (and (equal? (vector-ref a i) (vector-ref b i))
                         (loop (add1 i))))))]
        [else #f]))

;; ───────── numbers ─────────

(define (abs n) (if (< n 0) (- n) n))
(define (min a b) (if (< a b) a b))
(define (max a b) (if (> a b) a b))
(define (even? n) (zero? (remainder n 2)))
(define (odd? n) (not (even? n)))
(define (gcd a b) (if (zero? b) (abs a) (gcd b (remainder a b))))
(define (expt base n) (if (zero? n) 1 (* base (expt base (sub1 n)))))

(define (%digit r) (integer->char (+ 48 (abs r))))

;; works on negative numbers directly so the most negative fixnum is fine too
(define (number->string n)
  (let loop ([q (quotient n 10)] [acc (list (%digit (remainder n 10)))])
    (if (zero? q)
        (list->string (if (< n 0) (cons #\- acc) acc))
        (loop (quotient q 10) (cons (%digit (remainder q 10)) acc)))))

;; ───────── strings, symbols, vectors ─────────

(define (string=? a b)
  (and (= (string-length a) (string-length b))
       (let loop ([i 0])
         (or (= i (string-length a))
             (and (char=? (string-ref a i) (string-ref b i))
                  (loop (add1 i)))))))

(define (substring s start end)
  (let ([r (make-string (- end start))])
    (let loop ([i start])
      (cond [(< i end)
             (string-set! r (- i start) (string-ref s i))
             (loop (add1 i))]
            [else r]))))

(define (string-copy s) (substring s 0 (string-length s)))

(define (string-append a b)
  (let* ([la (string-length a)]
         [r (make-string (+ la (string-length b)))])
    (let loop ([i 0])
      (cond [(< i la) (string-set! r i (string-ref a i)) (loop (add1 i))]
            [(< i (string-length r)) (string-set! r i (string-ref b (- i la))) (loop (add1 i))]
            [else r]))))

(define (string->list s)
  (let loop ([i (sub1 (string-length s))] [acc '()])
    (if (< i 0) acc (loop (sub1 i) (cons (string-ref s i) acc)))))

(define (list->string l)
  (let ([r (make-string (length l))])
    (let loop ([l l] [i 0])
      (cond [(null? l) r]
            [else (string-set! r i (car l)) (loop (cdr l) (add1 i))]))))

;; symbols are immutable, so hand out a fresh copy of the name
(define (symbol->string s) (string-copy (%symbol-chars s)))

(define (vector->list v)
  (let loop ([i (sub1 (vector-length v))] [acc '()])
    (if (< i 0) acc (loop (sub1 i) (cons (vector-ref v i) acc)))))

(define (list->vector l)
  (let ([v (make-vector (length l))])
    (let loop ([l l] [i 0])
      (cond [(null? l) v]
            [else (vector-set! v i (car l)) (loop (cdr l) (add1 i))]))))
