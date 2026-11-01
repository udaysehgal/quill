#!/bin/sh
# End-to-end tests: compile a program, run it, compare stdout.
set -e
cd "$(dirname "$0")/.."
cargo build -q
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
pass=0; fail=0

check() { # check <program> <expected output>
  # runs twice: normally, and with a 1 KB starting heap so the GC runs constantly
  printf '%s' "$1" > "$tmp/t.scm"
  if ./target/debug/schemec "$tmp/t.scm" -o "$tmp/t" 2>"$tmp/err" \
     && got=$("$tmp/t" 2>"$tmp/err") && [ "$got" = "$2" ] \
     && got=$(SCHEMEC_HEAP_KB=1 "$tmp/t" 2>"$tmp/err") && [ "$got" = "$2" ]; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); echo "FAIL: $1"; echo "   expected: $2"; echo "   got:      $got $(cat "$tmp/err")"
  fi
  got=
}

check_error() { # check_error <program> <substring of the compile error>
  printf '%s' "$1" > "$tmp/t.scm"
  if ! ./target/debug/schemec "$tmp/t.scm" -o "$tmp/t" 2>"$tmp/err" && grep -qF -- "$2" "$tmp/err"; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); echo "FAIL (expected compile error '$2'): $1"; cat "$tmp/err"
  fi
}

check_runtime_error() { # check_runtime_error <program> <substring of stderr>
  printf '%s' "$1" > "$tmp/t.scm"
  if ./target/debug/schemec "$tmp/t.scm" -o "$tmp/t" 2>"$tmp/err" \
     && ! "$tmp/t" >/dev/null 2>"$tmp/err" && grep -qF -- "$2" "$tmp/err"; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); echo "FAIL (expected runtime error '$2'): $1"; cat "$tmp/err"
  fi
}

# ── integers
check '42' '42'
check '0' '0'
check '-7' '-7'
check '123456789012' '123456789012'
check '2305843009213693951' '2305843009213693951'    # largest fixnum
check '-2305843009213693952' '-2305843009213693952'  # smallest fixnum
check_error '2305843009213693952' "doesn't fit"

# ── other immediates
check '#t' '#t'
check '#f' '#f'
check '#\a' '#\a'
check '#\Z' '#\Z'
check '#\(' '#\('
check '#\space' '#\space'
check "'()" '()'

# ── unary primitives
check '(add1 41)' '42'
check '(sub1 (sub1 0))' '-2'
check '(zero? 0)' '#t'
check '(zero? 3)' '#f'
check '(not #f)' '#t'
check '(not 0)' '#f'
check '(integer? 5)' '#t'
check '(integer? #\a)' '#f'
check '(boolean? #f)' '#t'
check '(boolean? #t)' '#t'
check '(boolean? 0)' '#f'
check '(char? #\x)' '#t'
check '(char? 15)' '#f'
check "(null? '())" '#t'
check '(null? #f)' '#f'
check '(char->integer #\A)' '65'
check '(integer->char 97)' '#\a'

# ── binary primitives
check '(+ 1 2)' '3'
check '(- 10 3)' '7'
check '(- 3 10)' '-7'
check '(* 6 7)' '42'
check '(* -3 4)' '-12'
check '(+ 1 2 3 4)' '10'
check '(- 10 1 2)' '7'
check '(- 5)' '-5'
check '(*)' '1'
check '(+ (* 2 3) (- 10 (add1 2)))' '13'
check '(= 3 3)' '#t'
check '(< 1 2)' '#t'
check '(< 2 1)' '#f'
check '(> -1 -2)' '#t'
check '(<= 2 2)' '#t'
check '(>= 1 2)' '#f'
check '(eq? #\a #\a)' '#t'

# ── if / and / or / cond
check '(if #t 1 2)' '1'
check '(if #f 1 2)' '2'
check '(if 0 1 2)' '1'                          # only #f is false
check '(if (< 1 2) (+ 10 1) (+ 20 2))' '11'
check '(if #f 1)' ''                            # void prints nothing
check '(and)' '#t'
check '(and 1 2 3)' '3'
check '(and 1 #f 3)' '#f'
check '(or)' '#f'
check '(or #f 7)' '7'
check '(or #f #f)' '#f'
check '(cond [(= 1 2) 10] [(= 1 1) 20] [else 30])' '20'
check '(cond [#f 1] [else 2])' '2'
check '(cond [#f 1])' ''

# ── let / let*
check '(let ([x 5]) x)' '5'
check '(let ([x 5] [y 6]) (* x y))' '30'
check '(let ([x 1]) (let ([x 2]) x))' '2'                    # shadowing
check '(let ([x 1]) (let ([x (+ x 10)]) x))' '11'            # rhs sees outer x
check '(let ([x 1]) (let ([y 2]) (+ x y)))' '3'
check '(let ([x 1]) (let ([x 2] [y x]) y))' '1'              # let is parallel
check '(let* ([x 1] [y (+ x 1)] [z (* y 10)]) z)' '20'      # let* is sequential
check '(let ([a 1] [b 2] [c 3]) (+ (* a 100) (* b 10) c))' '123'
check_error '(let ([x 1]) y)' 'unbound variable: y'

# ── pairs and lists
check '(cons 1 2)' '(1 . 2)'
check "(cons 1 '())" '(1)'
check '(car (cons 1 2))' '1'
check '(cdr (cons 1 2))' '2'
check '(list 1 2 3)' '(1 2 3)'
check "'(1 2 3)" '(1 2 3)'
check "'(1 (2 3) #t #\\a)" '(1 (2 3) #t #\a)'
check '(cons 1 (cons 2 3))' '(1 2 . 3)'
check '(pair? (cons 1 2))' '#t'
check "(pair? '())" '#f'
check "(null? (cdr (list 1)))" '#t'
check '(let ([p (cons 1 2)]) (+ (car p) (cdr p)))' '3'
check '(car (cdr (list 10 20 30)))' '20'

# ── functions
check '(define (sq x) (* x x)) (sq 7)' '49'
check '(define (f) 42) (f)' '42'
check '(define (fact n) (if (= n 0) 1 (* n (fact (- n 1))))) (fact 19)' '121645100408832000'
check '(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) (fib 25)' '75025'
check '(define (sub a b) (- a b)) (sub 10 3)' '7'                         # argument order
check '(define (g x) (* x 10)) (define (f a b) (+ a b)) (f (g 1) (g 2))' '30'  # nested calls as args
check '(define (f a b c d e f2 g h) (list a b c d e f2 g h)) (f 1 2 3 4 5 6 7 8)' '(1 2 3 4 5 6 7 8)'
check '(define (later) (sooner)) (define (sooner) 5) (later)' '5'        # define order doesn't matter
check '(define (f x) (let ([y (* x 2)]) (+ x y))) (f 5)' '15'
check '(define (id x) x) (let ([a 1] [b (id 2)]) (+ a b (id 3)))' '6'     # call results don't clobber locals
check '(define (f x) x) 1 2 (f 3)' '3'                                   # last expression is printed
check '(define (sum-to n) (if (= n 0) 0 (+ n (sum-to (- n 1))))) (sum-to 10000)' '50005000'

# ── lists with recursion
check '(define (len l) (if (null? l) 0 (add1 (len (cdr l))))) (len (list 1 2 3 4))' '4'
check '(define (app a b) (if (null? a) b (cons (car a) (app (cdr a) b)))) (app (list 1 2) (list 3 4))' '(1 2 3 4)'
check '(define (rev l acc) (if (null? l) acc (rev (cdr l) (cons (car l) acc)))) (rev (list 1 2 3) (quote ()))' '(3 2 1)'
check '(define (range a b) (if (>= a b) (quote ()) (cons a (range (add1 a) b)))) (range 0 5)' '(0 1 2 3 4)'

# ── tail calls (these overflow the 8 MB stack without proper tail calls)
check '(define (loop n acc) (if (= n 0) acc (loop (- n 1) (+ acc 1)))) (loop 10000000 0)' '10000000'
check '(define (ev? n) (if (= n 0) #t (od? (- n 1))))
       (define (od? n) (if (= n 0) #f (ev? (- n 1))))
       (ev? 1000001)' '#f'
check '(define (count n) (cond [(= n 0) 0] [else (count (sub1 n))])) (count 5000000)' '0'
check '(define (build n acc) (if (= n 0) acc (build (- n 1) (cons n acc))))
       (define (all-pos? l) (or (null? l) (and (> (car l) 0) (all-pos? (cdr l)))))
       (all-pos? (build 1000000 (quote ())))' '#t'
check '(define (loop n) (let ([m (- n 1)]) (if (= m 0) 99 (loop m)))) (loop 5000000)' '99'

# ── closures
check '((lambda (x) (* x 2)) 21)' '42'
check '(let ([add (lambda (a b) (+ a b))]) (add 1 2))' '3'
check '(define (make-adder n) (lambda (x) (+ x n))) ((make-adder 10) 5)' '15'
check '(define (compose f g) (lambda (x) (f (g x)))) ((compose add1 add1) 1)' '3'   # primitives as values
check '(let ([x 1]) (let ([f (lambda () x)]) (let ([x 2]) (f))))' '1'               # lexical scope
check '(define (f) (lambda (a) (lambda (b) (lambda (c) (list a b c))))) ((((f) 1) 2) 3)' '(1 2 3)'
check '(lambda (x) x)' '#<procedure>'
check '(define (f x) x) f' '#<procedure>'
check '(procedure? car)' '#t'
check '(procedure? (lambda () 1))' '#t'
check '(procedure? 5)' '#f'
check '(define (sq x) (* x x)) (map sq (list 1 2 3))' '(1 4 9)'
check '(map (lambda (x) (* x 10)) (list 1 2 3))' '(10 20 30)'
check '(map car (list (list 1 2) (list 3 4)))' '(1 3)'
check '(fold-left + 0 (list 1 2 3 4 5))' '15'
check '(fold-right cons (quote ()) (list 1 2 3))' '(1 2 3)'
check '(filter (lambda (x) (> x 2)) (list 1 2 3 4))' '(3 4)'
check '(let ([f (lambda (f n) (if (= n 0) 1 (* n (f f (- n 1)))))]) (f f 10))' '3628800'
check '(define (f x) (x 1)) (f add1)' '2'
check '(define (apply-n f n x) (if (= n 0) x (apply-n f (sub1 n) (f x)))) (apply-n (lambda (x) (* x 2)) 10 1)' '1024'
check '(define (loop n) (if (= n 0) 0 ((lambda () (loop (- n 1)))))) (loop 1000000)' '0'  # tail call through a closure

# ── set! and boxes
check '(let ([x 1]) (set! x 2) x)' '2'
check '(let ([x 1]) (set! x (+ x 1)) (set! x (* x 10)) x)' '20'
check '(define (counter) (let ([n 0]) (lambda () (set! n (add1 n)) n))) (let ([c (counter)]) (c) (c) (c))' '3'
check '(define (counter) (let ([n 0]) (lambda () (set! n (add1 n)) n))) (let ([a (counter)] [b (counter)]) (a) (a) (b) (list (a) (b)))' '(3 2)'
check '(let ([x 1]) (let ([get (lambda () x)] [set (lambda (v) (set! x v))]) (set 42) (get)))' '42'   # shared box
check '(define (f x) (set! x (* x 2)) x) (f 21)' '42'
check '(define g 1) (set! g 2) g' '2'
check '(define total 0) (define (add! n) (set! total (+ total n))) (add! 5) (add! 7) total' '12'
check '(let ([x 1]) (set! x 2))' ''

# ── letrec, named let, internal define, when/unless
check '(letrec ([ev? (lambda (n) (if (= n 0) #t (od? (- n 1))))] [od? (lambda (n) (if (= n 0) #f (ev? (- n 1))))]) (ev? 100))' '#t'
check '(let loop ([i 0] [acc (quote ())]) (if (= i 5) acc (loop (add1 i) (cons i acc))))' '(4 3 2 1 0)'
check '(let loop ([i 0]) (if (< i 3000000) (loop (add1 i)) i))' '3000000'
check '(define (f x) (define y (* x 2)) (define (g z) (+ y z)) (g 1)) (f 10)' '21'
check '(define (f) (define (ev? n) (if (= n 0) #t (od? (- n 1)))) (define (od? n) (if (= n 0) #f (ev? (- n 1)))) (od? 7)) (f)' '#t'
check '(let () (define x 5) (* x x))' '25'
check '(when (> 1 0) 1 2)' '2'
check '(unless (> 1 0) 1)' ''
check '(let* () 5)' '5'

# ── global defines
check '(define x 10) (define y (* x 2)) (+ x y)' '30'
check '(define sq (lambda (x) (* x x))) (sq 9)' '81'
check '(define (f) x) (define x 5) (f)' '5'
check '(define (f x) x)' ''
check '(define lst (list 1 2 3)) (length lst)' '3'
check '(define (length l) 99) (length (list 1))' '99'    # user definitions override the library

# ── strings, symbols, vectors
check '"hello"' '"hello"'
check '(string-length "hello")' '5'
check '(string-ref "hello" 1)' '#\e'
check '(let ([s (make-string 3 #\a)]) (string-set! s 1 #\b) s)' '"aba"'
check '(string-append "foo" "bar")' '"foobar"'
check '(substring "hello world" 6 11)' '"world"'
check '(string=? "abc" "abc")' '#t'
check '(string=? "abc" "abd")' '#f'
check '(string->list "abc")' '(#\a #\b #\c)'
check '(list->string (list #\h #\i))' '"hi"'
check '(number->string 1234)' '"1234"'
check '(number->string -56)' '"-56"'
check '(number->string 0)' '"0"'
check '(number->string -2305843009213693952)' '"-2305843009213693952"'
check '"a \"quoted\" word\n"' '"a \"quoted\" word\n"'
check '(string? "x")' '#t'
check '(quote hello)' 'hello'
check '(eq? (quote abc) (quote abc))' '#t'
check '(eq? (quote abc) (quote abd))' '#f'
check '(symbol? (quote a))' '#t'
check '(symbol->string (quote apple))' '"apple"'
check '(quote (a (b c) "s" 1))' '(a (b c) "s" 1)'
check '(assq (quote b) (quote ((a 1) (b 2))))' '(b 2)'
check '(memq (quote c) (quote (a b c d)))' '(c d)'
check '(vector 1 2 3)' '#(1 2 3)'
check '(make-vector 3 0)' '#(0 0 0)'
check '(vector)' '#()'
check '(let ([v (make-vector 3 0)]) (vector-set! v 0 (quote x)) (vector-set! v 2 "z") v)' '#(x 0 "z")'
check '(vector-ref (vector 1 2 3) 2)' '3'
check '(vector-length (make-vector 100))' '100'
check '(vector->list (vector 1 2))' '(1 2)'
check '(list->vector (list 1 2))' '#(1 2)'
check '(equal? (list 1 (vector 2 "x")) (list 1 (vector 2 "x")))' '#t'
check '(equal? (list 1 2) (list 1 3))' '#f'
check '(member (list 2) (list (list 1) (list 2) (list 3)))' '((2) (3))'
check '(let ([p (cons 1 2)]) (set-car! p 10) (set-cdr! p 20) p)' '(10 . 20)'

# ── numbers
check '(quotient 17 5)' '3'
check '(quotient -17 5)' '-3'
check '(remainder 17 5)' '2'
check '(remainder -17 5)' '-2'
check '(modulo -17 5)' '3'
check '(modulo 17 -5)' '-3'
check '(modulo 15 5)' '0'
check '(abs -5)' '5'
check '(max 3 7)' '7'
check '(even? 10)' '#t'
check '(odd? 7)' '#t'
check '(gcd 12 18)' '6'
check '(expt 2 10)' '1024'
check '(char<? #\a #\b)' '#t'

# ── output
check '(display "hi") (newline) (display 42)' 'hi
42'
check '(begin (display "x = ") (display 5) (newline) 7)' 'x = 5
7'
check '(for-each (lambda (x) (display x) (display " ")) (list 1 2 3))' '1 2 3 '
check '(write "a") (display "a") (write #\b) (display #\b)' '"a"a#\bb'
check '(display (list 1 "two" #\3 (quote four)))' '(1 two 3 four)'

# ── garbage collection (also run with a 1 KB starting heap by check)
check '(define (build n acc) (if (= n 0) acc (build (- n 1) (cons n acc))))
       (define (sum l acc) (if (null? l) acc (sum (cdr l) (+ acc (car l)))))
       (define (go i total) (if (= i 0) total (go (- i 1) (+ total (sum (build 10000 (quote ())) 0)))))
       (go 200 0)' '10001000000'
check '(let loop ([i 0] [keep (quote ())])   ; churn through garbage while keeping a live list
         (if (= i 200000) (length keep)
             (loop (add1 i) (if (= 0 (remainder i 1000)) (cons (make-vector 10 i) keep) keep))))' '200'
check '(define v (make-vector 1000 0))   ; globals are GC roots
       (let loop ([i 0]) (when (< i 1000) (vector-set! v i (list i)) (loop (add1 i))))
       (let loop ([i 0]) (when (< i 100000) (cons i i) (loop (add1 i))))
       (car (vector-ref v 999))' '999'
check '(define (make-counter) (let ([n 0]) (lambda () (set! n (add1 n)) n)))   ; closures and boxes survive GC
       (let ([c (make-counter)])
         (let loop ([i 0]) (when (< i 100000) (c) (cons 1 2) (loop (add1 i))))
         (c))' '100001'
check '(let loop ([i 0] [s ""]) (if (= i 300) (string-length s) (loop (add1 i) (string-append s "ab"))))' '600'
check '(define (tree n) (if (= n 0) (quote ()) (list (tree (- n 1)) (tree (- n 1)))))
       (define (count t) (if (null? t) 1 (+ (count (car t)) (count (car (cdr t))))))
       (count (tree 16))' '65536'

# ── runtime errors
check_runtime_error '(car 5)' 'car: expected a pair, got 5'
check_runtime_error '(cdr (quote ()))' 'cdr: expected a pair, got ()'
check_runtime_error '(+ 1 #t)' '+: expected an integer, got #t'
check_runtime_error '(+ "a" 1)' '+: expected an integer, got "a"'
check_runtime_error '(vector-ref (vector 1 2) 2)' 'vector-ref: index out of range: 2'
check_runtime_error '(vector-ref (vector 1 2) -1)' 'index out of range: -1'
check_runtime_error '(string-ref "abc" 10)' 'string-ref: index out of range: 10'
check_runtime_error '(5 1)' 'not a procedure: 5'
check_runtime_error '((lambda (x) x) 1 2)' 'lambda: expected 1 argument(s), got 2'
check_runtime_error '(define (f x) x) (let ([g f]) (g))' 'f: expected 1 argument(s), got 0'
check_runtime_error '(quotient 1 0)' 'quotient: division by zero'
check_runtime_error '(error "bad thing:" 42)' 'error: bad thing: 42'
check_runtime_error '(define (f) y) (define y (f)) 1' 'y: variable used before its definition'
check_runtime_error '(display "partial") (car 1)' 'car: expected a pair'
check_runtime_error '(make-vector -1)' 'make-vector: expected a non-negative length, got -1'
check_runtime_error '(symbol->string "s")' 'symbol->string: expected a symbol'

# ── more compile errors
check_error '(lambda (x x) x)' 'duplicate parameter'
check_error '(let ([if 1]) if)' 'keyword'
check_error '(set! nope 1)' 'unbound variable: nope'
check_error '(define (f) 1) (set! f 2)' 'defined as a function'
check_error '(let ([x 1] [x 2]) x)' 'duplicate binding'
check_error '(map list (list 1))' "can't be used as a value"
check_error '(let ([x 1]) (define y 2))' 'at least one expression'
check_error '(if)' 'if expects'

# ── errors
check_error '(define (f x) x) (f 1 2)' 'expects 1 argument'
check_error '(define (f x) x) (define (f y) y) (f 1)' 'defined twice'
check_error '(define (if x) x) 1' 'keyword'
check_error '(define (f x x) x) 1' 'duplicate parameter'
check_error '(foo 1)' 'unbound variable: foo'
check_error '(add1 1 2)' 'expects 1 argument'

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
