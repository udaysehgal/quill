// schemec runtime: heap + copying garbage collector, printing, errors.
// Tag constants must match src/compiler.rs.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int64_t value;

#define CHAR_TAG     0x0F
#define CHAR_SHIFT   8
#define FALSE_V      0x2F
#define TRUE_V       0x6F
#define EMPTY_V      0x3F
#define VOID_V       0x4F
#define FORWARD      0x1F   // marks an object the GC already moved (never a real value)
#define PAIR_TAG     1
#define CLOSURE_TAG  2
#define SYMBOL_TAG   3
#define VECTOR_TAG   5
#define STRING_TAG   6

#define IS_FIXNUM(v)  (((v) & 3) == 0)
#define IS_POINTER(v) (!IS_FIXNUM(v) && ((v) & 7) != 7)
#define TAG(v)        ((v) & 7)
#define FIELDS(v)     ((value *)((v) - TAG(v)))
#define UNFIX(v)      ((v) >> 2)

// defined by the compiled program
extern value scheme_entry(char *heap, char *heap_end);
extern uintptr_t scm_stack_base;
extern value scm_globals_start[], scm_globals_end[];

// ───────────────────────────── garbage collector ─────────────────────────────
//
// Two-space copying collector (Cheney-style, with an explicit worklist since
// heap objects other than vectors/strings/closures have no header).
// Roots: every slot of every Scheme stack frame, plus global variables.

static char *heap_start, *heap_end;   // current space
static char *to_top;                  // bump pointer while copying
static size_t gc_count;

static value *worklist;
static size_t wl_len, wl_cap;

static void wl_push(value v) {
    if (wl_len == wl_cap) {
        wl_cap = wl_cap ? wl_cap * 2 : 1024;
        worklist = realloc(worklist, wl_cap * sizeof(value));
        if (!worklist) { fprintf(stderr, "out of memory\n"); exit(1); }
    }
    worklist[wl_len++] = v;
}

static size_t round16(size_t n) { return (n + 15) & ~(size_t)15; }

static size_t object_size(value v) {
    value *p = FIELDS(v);
    switch (TAG(v)) {
    case PAIR_TAG:    return 16;
    case CLOSURE_TAG: return round16(16 + 8 * (size_t)p[1]);
    case VECTOR_TAG:  return round16(8 + 8 * (size_t)UNFIX(p[0]));
    case STRING_TAG:  return round16(8 + (size_t)UNFIX(p[0]));
    default:
        fprintf(stderr, "gc: bad heap object 0x%llx\n", (unsigned long long)v);
        exit(1);
    }
}

static value copy(value v) {
    if (!IS_POINTER(v)) return v;
    value *p = FIELDS(v);
    if ((char *)p < heap_start || (char *)p >= heap_end) return v;  // static data
    if (p[0] == FORWARD) return p[1];
    size_t size = object_size(v);
    value *np = (value *)to_top;
    to_top += size;
    memcpy(np, p, size);
    value nv = (value)np + TAG(v);
    p[0] = FORWARD;
    p[1] = nv;
    if (TAG(v) != STRING_TAG) wl_push(nv);
    return nv;
}

static void scan(value v) {
    value *p = FIELDS(v);
    switch (TAG(v)) {
    case PAIR_TAG:
        p[0] = copy(p[0]);
        p[1] = copy(p[1]);
        break;
    case CLOSURE_TAG:   // [code, nfree, free...]: the first two aren't values
        for (value i = 0; i < p[1]; i++) p[2 + i] = copy(p[2 + i]);
        break;
    case VECTOR_TAG:
        for (value i = 0; i < UNFIX(p[0]); i++) p[1 + i] = copy(p[1 + i]);
        break;
    }
}

// Copy everything reachable into a fresh space of `size` bytes.
static void collect(value *sp, value *fp, size_t size) {
    char *space = malloc(size);   // 16-byte aligned on macOS
    if (!space) { fprintf(stderr, "out of memory (heap of %zu bytes)\n", size); exit(1); }
    to_top = space;

    // a frame's slots are [sp, fp); the caller's sp is just above our saved x29/x30
    while ((uintptr_t)fp != scm_stack_base) {
        for (value *s = sp; s < fp; s++) *s = copy(*s);
        sp = fp + 2;
        fp = (value *)fp[0];
    }
    for (value *g = scm_globals_start; g < scm_globals_end; g++) *g = copy(*g);
    while (wl_len) scan(worklist[--wl_len]);

    free(heap_start);
    heap_start = space;
    heap_end = space + size;
}

typedef struct { char *ptr, *limit; } heap_state;

// Called from scm_alloc when `need` bytes don't fit.
heap_state scheme_gc(value *sp, value *fp, size_t need) {
    size_t size = heap_end - heap_start;
    collect(sp, fp, size);
    size_t live = to_top - heap_start;
    if (live + need > size / 2) {   // keep at least half the heap free: grow
        while (live + need > size / 2) size *= 2;
        collect(sp, fp, size);
    }
    gc_count++;
    return (heap_state){to_top, heap_end};
}

// ───────────────────────────── printing ─────────────────────────────

static void print(FILE *out, value v, int write);

static void print_string(FILE *out, value v, int write) {
    value *p = FIELDS(v);
    const char *s = (const char *)(p + 1);
    size_t len = UNFIX(p[0]);
    if (!write) { fwrite(s, 1, len, out); return; }
    fputc('"', out);
    for (size_t i = 0; i < len; i++) {
        switch (s[i]) {
        case '"':  fputs("\\\"", out); break;
        case '\\': fputs("\\\\", out); break;
        case '\n': fputs("\\n", out); break;
        case '\t': fputs("\\t", out); break;
        default:   fputc(s[i], out);
        }
    }
    fputc('"', out);
}

static void print_char(FILE *out, char c, int write) {
    if (!write) { fputc(c, out); return; }
    switch (c) {
    case ' ':  fputs("#\\space", out); break;
    case '\n': fputs("#\\newline", out); break;
    case '\t': fputs("#\\tab", out); break;
    case '\0': fputs("#\\nul", out); break;
    default:   fprintf(out, "#\\%c", c);
    }
}

static void print(FILE *out, value v, int write) {
    if (IS_FIXNUM(v)) {
        fprintf(out, "%lld", (long long)UNFIX(v));
    } else if ((v & 0xFF) == CHAR_TAG) {
        print_char(out, (char)(v >> CHAR_SHIFT), write);
    } else if (v == FALSE_V) {
        fputs("#f", out);
    } else if (v == TRUE_V) {
        fputs("#t", out);
    } else if (v == EMPTY_V) {
        fputs("()", out);
    } else if (v == VOID_V) {
        fputs("#<void>", out);
    } else if (TAG(v) == PAIR_TAG) {
        fputc('(', out);
        print(out, FIELDS(v)[0], write);
        value rest = FIELDS(v)[1];
        while (TAG(rest) == PAIR_TAG && !IS_FIXNUM(rest)) {
            fputc(' ', out);
            print(out, FIELDS(rest)[0], write);
            rest = FIELDS(rest)[1];
        }
        if (rest != EMPTY_V) {
            fputs(" . ", out);
            print(out, rest, write);
        }
        fputc(')', out);
    } else if (TAG(v) == VECTOR_TAG) {
        value *p = FIELDS(v);
        fputs("#(", out);
        for (value i = 0; i < UNFIX(p[0]); i++) {
            if (i) fputc(' ', out);
            print(out, p[1 + i], write);
        }
        fputc(')', out);
    } else if (TAG(v) == STRING_TAG) {
        print_string(out, v, write);
    } else if (TAG(v) == SYMBOL_TAG) {
        print_string(out, v - SYMBOL_TAG + STRING_TAG, 0);
    } else if (TAG(v) == CLOSURE_TAG) {
        fputs("#<procedure>", out);
    } else {
        fprintf(out, "#<unknown 0x%llx>", (unsigned long long)v);
    }
}

void scheme_display(value v) { print(stdout, v, 0); }
void scheme_write(value v) { print(stdout, v, 1); }

// ───────────────────────────── errors ─────────────────────────────

// Runtime type errors etc. `culprit` is the offending value (or void for none).
void scheme_error(const char *msg, value culprit) {
    fflush(stdout);
    fprintf(stderr, "error: %s", msg);
    if (culprit != VOID_V) {
        fputc(' ', stderr);
        print(stderr, culprit, 1);
    }
    fputc('\n', stderr);
    exit(1);
}

// (error msg) / (error msg irritant)
void scheme_user_error(value msg, value irritant) {
    fflush(stdout);
    fputs("error: ", stderr);
    print(stderr, msg, 0);
    if (irritant != VOID_V) {
        fputc(' ', stderr);
        print(stderr, irritant, 1);
    }
    fputc('\n', stderr);
    exit(1);
}

// ───────────────────────────── main ─────────────────────────────

int main(void) {
    // start small and let the GC grow the heap; SCHEMEC_HEAP_KB overrides
    size_t size = 256 * 1024;
    const char *env = getenv("SCHEMEC_HEAP_KB");
    if (env && atol(env) > 0) size = round16((size_t)atol(env) * 1024);
    heap_start = malloc(size);
    if (!heap_start) { fprintf(stderr, "could not allocate heap\n"); return 1; }
    heap_end = heap_start + size;

    value result = scheme_entry(heap_start, heap_end);
    if (result != VOID_V) {
        print(stdout, result, 1);
        putchar('\n');
    }
    if (getenv("SCHEMEC_GC_STATS"))
        fprintf(stderr, "gc: %zu collections, final heap %zu KiB\n", gc_count,
                (size_t)(heap_end - heap_start) / 1024);
    return 0;
}
