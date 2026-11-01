// The C runtime: allocates the heap, calls the compiled code, prints the result.
// Tag constants must match src/compiler.rs.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#define FIXNUM_MASK 0x3
#define CHAR_MASK   0xFF
#define CHAR_TAG    0x0F
#define CHAR_SHIFT  8
#define FALSE_V     0x2F
#define TRUE_V      0x6F
#define EMPTY_V     0x3F
#define PAIR_MASK   0x7
#define PAIR_TAG    0x1

#define HEAP_SIZE (64 * 1024 * 1024) // no GC yet, so this is all you get

typedef int64_t value;

extern value scheme_entry(void *heap);

static value car(value p) { return ((value *)(p - PAIR_TAG))[0]; }
static value cdr(value p) { return ((value *)(p - PAIR_TAG))[1]; }

static void print_char(char c) {
    switch (c) {
    case ' ':  printf("#\\space"); break;
    case '\n': printf("#\\newline"); break;
    case '\t': printf("#\\tab"); break;
    case '\0': printf("#\\nul"); break;
    default:   printf("#\\%c", c);
    }
}

static void print(value v) {
    if ((v & FIXNUM_MASK) == 0) {
        printf("%lld", (long long)(v >> 2));
    } else if ((v & CHAR_MASK) == CHAR_TAG) {
        print_char((char)(v >> CHAR_SHIFT));
    } else if (v == FALSE_V) {
        printf("#f");
    } else if (v == TRUE_V) {
        printf("#t");
    } else if (v == EMPTY_V) {
        printf("()");
    } else if ((v & PAIR_MASK) == PAIR_TAG) {
        printf("(");
        print(car(v));
        value rest = cdr(v);
        while ((rest & PAIR_MASK) == PAIR_TAG) {
            printf(" ");
            print(car(rest));
            rest = cdr(rest);
        }
        if (rest != EMPTY_V) {
            printf(" . ");
            print(rest);
        }
        printf(")");
    } else {
        printf("#<unknown 0x%llx>", (unsigned long long)v);
    }
}

int main(void) {
    void *heap = calloc(1, HEAP_SIZE); // malloc on macOS is 16-byte aligned
    if (!heap) {
        fprintf(stderr, "could not allocate heap\n");
        return 1;
    }
    print(scheme_entry(heap));
    printf("\n");
    free(heap);
    return 0;
}
