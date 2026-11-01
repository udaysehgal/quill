#!/bin/sh
# End-to-end tests: compile a program, run it, compare stdout.
set -e
cd "$(dirname "$0")/.."
cargo build -q
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
pass=0; fail=0

check() { # check <program> <expected output>
  printf '%s' "$1" > "$tmp/t.scm"
  if ./target/debug/schemec "$tmp/t.scm" -o "$tmp/t" 2>"$tmp/err" && got=$("$tmp/t") && [ "$got" = "$2" ]; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); echo "FAIL: $1"; echo "   expected: $2"; echo "   got:      ${got:-$(cat "$tmp/err")}"
  fi
  got=
}

check_error() { # check_error <program> <substring of the compile error>
  printf '%s' "$1" > "$tmp/t.scm"
  if ! ./target/debug/schemec "$tmp/t.scm" -o "$tmp/t" 2>"$tmp/err" && grep -qF "$2" "$tmp/err"; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); echo "FAIL (expected compile error '$2'): $1"; cat "$tmp/err"
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
check '(if #f 1)' '#f'
check '(and)' '#t'
check '(and 1 2 3)' '3'
check '(and 1 #f 3)' '#f'
check '(or)' '#f'
check '(or #f 7)' '7'
check '(or #f #f)' '#f'
check '(cond [(= 1 2) 10] [(= 1 1) 20] [else 30])' '20'
check '(cond [#f 1] [else 2])' '2'
check '(cond [#f 1])' '#f'

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

# ── errors
check_error '(define (f x) x) (f 1 2)' 'expects 1 argument'
check_error '(define (f x) x) (define (f y) y) (f 1)' 'defined twice'
check_error '(define (f x) x) f' 'is a function'
check_error '(define (f x) (x 1)) (f 1)' 'is a variable'
check_error '(define (if x) x) 1' "can't redefine"
check_error '(define (f x x) x) 1' 'duplicate parameter'
check_error '(define (f x) x)' 'no expression'
check_error '(let ([x 1]) (define (f) 1) 2)' 'top level'
check_error '(foo 1)' 'unknown function'
check_error '(add1 1 2)' 'expects 1 argument'
check_error '(1 2)' "can't apply"

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
