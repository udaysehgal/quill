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

# ── errors
check_error '(foo 1)' 'unknown function'
check_error '(add1 1 2)' 'expects 1 argument'
check_error '(1 2)' "can't apply"

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
