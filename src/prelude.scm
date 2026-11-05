;;; quill's standard library, written in the Scheme quill itself compiles.
;;; It is prepended to every program, but only the definitions a program
;;; (transitively) uses are compiled. A user definition with the same name wins.
;;; Names starting with % are internal helpers.

;; ───────── lists ─────────

(define (length l)
  (let loop ([l l] [n 0])
    (if (null? l) n (loop (cdr l) (add1 n)))))

(define (%append2 a b)
  (if (null? a) b (cons (car a) (%append2 (cdr a) b))))

(define (append . ls)
  (cond [(null? ls) '()]
        [(null? (cdr ls)) (car ls)]
        [else (%append2 (car ls) (apply append (cdr ls)))]))

(define (list? x)
  (cond [(null? x) #t] [(pair? x) (list? (cdr x))] [else #f]))

(define (list-copy l) (%append2 l '()))

(define (last-pair l) (if (pair? (cdr l)) (last-pair (cdr l)) l))

(define (iota n)
  (let loop ([i (sub1 n)] [acc '()])
    (if (< i 0) acc (loop (sub1 i) (cons i acc)))))

(define (reverse l)
  (let loop ([l l] [acc '()])
    (if (null? l) acc (loop (cdr l) (cons (car l) acc)))))

(define (list-tail l k)
  (if (zero? k) l (list-tail (cdr l) (sub1 k))))

(define (list-ref l k) (car (list-tail l k)))

(define (%map1 f l)
  (if (null? l) '() (cons (f (car l)) (%map1 f (cdr l)))))

;; stops at the end of the shortest list
(define (%map-n f ls)
  (if (memq '() ls) '()
      (cons (apply f (%map1 car ls)) (%map-n f (%map1 cdr ls)))))

(define (map f l . ls)
  (if (null? ls) (%map1 f l) (%map-n f (cons l ls))))

(define (for-each f l . ls)
  (if (null? ls)
      (let loop ([l l])
        (unless (null? l) (f (car l)) (loop (cdr l))))
      (let loop ([ls (cons l ls)])
        (unless (memq '() ls)
          (apply f (%map1 car ls))
          (loop (%map1 cdr ls))))))

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

(define (memv x l) (memq x l))   ; eqv? is eq? for fixnums, chars and symbols
(define (assv x l) (assq x l))

;; stable merge sort
(define (sort l less?)
  (define (merge a b)
    (cond [(null? a) b]
          [(null? b) a]
          [(less? (car b) (car a)) (cons (car b) (merge a (cdr b)))]
          [else (cons (car a) (merge (cdr a) b))]))
  (define (split l a b)
    (if (null? l) (cons a b) (split (cdr l) b (cons (car l) a))))
  (if (or (null? l) (null? (cdr l)))
      l
      (let ([halves (split l '() '())])
        (merge (sort (reverse (car halves)) less?) (sort (reverse (cdr halves)) less?)))))

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
(define (min x . xs) (fold-left (lambda (a b) (if (< b a) b a)) x xs))
(define (max x . xs) (fold-left (lambda (a b) (if (> b a) b a)) x xs))
(define (number? x) (integer? x))
(define (positive? n) (> n 0))
(define (negative? n) (< n 0))
(define (square n) (* n n))
(define (even? n) (zero? (remainder n 2)))
(define (odd? n) (not (even? n)))
(define (gcd a b) (if (zero? b) (abs a) (gcd b (remainder a b))))
(define (expt base n) (if (zero? n) 1 (* base (expt base (sub1 n)))))

(define (%digit r) (integer->char (+ 48 (abs r))))

;; #f if s isn't a decimal integer
(define (string->number s)
  (let* ([n (string-length s)]
         [neg? (and (> n 1) (char=? (string-ref s 0) #\-))]
         [start (if (or neg? (and (> n 1) (char=? (string-ref s 0) #\+))) 1 0)])
    (let loop ([i start] [acc 0])
      (cond [(= i n) (and (> n start) (if neg? (- acc) acc))]
            [(char-numeric? (string-ref s i))
             (loop (add1 i) (+ (* acc 10) (- (char->integer (string-ref s i)) 48)))]
            [else #f]))))

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

(define (string-append . strings)
  (let ([r (make-string (fold-left (lambda (n s) (+ n (string-length s))) 0 strings))])
    (let copy ([strings strings] [at 0])
      (if (null? strings)
          r
          (let ([s (car strings)])
            (do ([i 0 (add1 i)]) ((= i (string-length s)))
              (string-set! r (+ at i) (string-ref s i)))
            (copy (cdr strings) (+ at (string-length s))))))))

(define (string . chars) (list->string chars))

(define (string<? a b)
  (let loop ([i 0])
    (cond [(= i (string-length b)) #f]
          [(= i (string-length a)) #t]
          [(char<? (string-ref a i) (string-ref b i)) #t]
          [(char<? (string-ref b i) (string-ref a i)) #f]
          [else (loop (add1 i))])))

(define (%string-map f s)
  (let ([r (make-string (string-length s))])
    (do ([i 0 (add1 i)]) ((= i (string-length s)) r)
      (string-set! r i (f (string-ref s i))))))

(define (string-upcase s) (%string-map char-upcase s))
(define (string-downcase s) (%string-map char-downcase s))

(define (char-alphabetic? c) (let ([d (char-downcase c)]) (and (char<=? #\a d) (char<=? d #\z))))
(define (char-numeric? c) (and (char<=? #\0 c) (char<=? c #\9)))
(define (char-whitespace? c) (and (memv c '(#\space #\tab #\newline)) #t))
(define (char-upper-case? c) (and (char<=? #\A c) (char<=? c #\Z)))
(define (char-lower-case? c) (and (char<=? #\a c) (char<=? c #\z)))
(define (char-upcase c) (if (char-lower-case? c) (integer->char (- (char->integer c) 32)) c))
(define (char-downcase c) (if (char-upper-case? c) (integer->char (+ (char->integer c) 32)) c))

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

(define (vector-map f v)
  (let ([r (make-vector (vector-length v))])
    (do ([i 0 (add1 i)]) ((= i (vector-length v)) r)
      (vector-set! r i (f (vector-ref v i))))))

(define (vector-for-each f v)
  (do ([i 0 (add1 i)]) ((= i (vector-length v)))
    (f (vector-ref v i))))

(define (vector-fill! v x)
  (do ([i 0 (add1 i)]) ((= i (vector-length v)))
    (vector-set! v i x)))

(define (%list->vector l) (list->vector l))

(define (list->vector l)
  (let ([v (make-vector (length l))])
    (let loop ([l l] [i 0])
      (cond [(null? l) v]
            [else (vector-set! v i (car l)) (loop (cdr l) (add1 i))]))))

;; ───────── variadic primitives used as values, e.g. (fold-left + 0 l) or (apply + l) ─────────

(define (%+ . xs) (let loop ([xs xs] [acc 0]) (if (null? xs) acc (loop (cdr xs) (+ acc (car xs))))))
(define (%* . xs) (let loop ([xs xs] [acc 1]) (if (null? xs) acc (loop (cdr xs) (* acc (car xs))))))
(define (%- x . xs)
  (if (null? xs) (- x) (let loop ([xs xs] [acc x]) (if (null? xs) acc (loop (cdr xs) (- acc (car xs)))))))
(define (%list . xs) xs)
(define (%vector . xs) (list->vector xs))

(define (%spread args)   ; (a b (c d)) -> (a b c d)
  (if (null? (cdr args)) (car args) (cons (car args) (%spread (cdr args)))))
(define (%apply f . args) (apply f (%spread args)))

;; ── continuations
;; %call/cc (assembly, in compiler.rs) captures the raw stack. %winders is the
;; list of (before . after) thunks of the dynamic-winds we are inside, innermost
;; first. Jumping to a continuation runs the afters of the extents we leave and
;; the befores of the ones we re-enter, outermost first.
(define %winders '())

(define (dynamic-wind before thunk after)
  (before)
  (set! %winders (cons (cons before after) %winders))
  (let ([result (thunk)])
    (set! %winders (cdr %winders))
    (after)
    result))

(define (%common-tail x y)
  (let ([lx (length x)] [ly (length y)])
    (let loop ([x (if (> lx ly) (list-tail x (- lx ly)) x)]
               [y (if (> ly lx) (list-tail y (- ly lx)) y)])
      (if (eq? x y) x (loop (cdr x) (cdr y))))))

(define (%travel-to to)
  (let ([common (%common-tail %winders to)])
    (let unwind ()
      (unless (eq? %winders common)
        (let ([after (cdr (car %winders))])
          (set! %winders (cdr %winders))
          (after))
        (unwind)))
    (let rewind ([l to])
      (unless (eq? l common)
        (rewind (cdr l))
        ((car (car l)))
        (set! %winders l)))))

(define (call-with-current-continuation f)
  (unless (procedure? f) (error "call/cc: expected a procedure, got" f))
  (%call/cc
   (lambda (k)
     (let ([winders %winders])
       (f (lambda args
            (unless (eq? %winders winders) (%travel-to winders))
            (apply k args)))))))

(define (call/cc f) (call-with-current-continuation f))
