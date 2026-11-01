// quill runtime: heap + copying garbage collector, printing, errors.
// Tag constants must match src/compiler.rs.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <sys/mman.h>
#include <unistd.h>

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
extern value scheme_entry(char *heap, char *heap_end, char *stack_top);
extern uintptr_t scm_stack_base;
extern value scm_globals_start[], scm_globals_end[];
extern value scm_symbols_start[], scm_symbols_end[];   // static (interned) symbols
extern value scm_argbuf[];                             // arguments beyond the 8th
extern int64_t scm_argbuf_live;                        // how many of them are roots right now

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
    for (int64_t i = 0; i < scm_argbuf_live; i++) scm_argbuf[i] = copy(scm_argbuf[i]);
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

static void put_utf8(FILE *out, uint32_t c) {
    if (c < 0x80) { fputc((int)c, out); return; }
    if (c < 0x800) { fputc(0xC0 | (c >> 6), out); }
    else {
        if (c < 0x10000) { fputc(0xE0 | (c >> 12), out); }
        else { fputc(0xF0 | (c >> 18), out); fputc(0x80 | ((c >> 12) & 0x3F), out); }
        fputc(0x80 | ((c >> 6) & 0x3F), out);
    }
    fputc(0x80 | (c & 0x3F), out);
}

static void print_char(FILE *out, uint32_t c, int write) {
    if (write) {
        switch (c) {
        case ' ':  fputs("#\\space", out); return;
        case '\n': fputs("#\\newline", out); return;
        case '\t': fputs("#\\tab", out); return;
        case '\0': fputs("#\\nul", out); return;
        default:   fputs("#\\", out);
        }
    }
    put_utf8(out, c);
}

static void print(FILE *out, value v, int write) {
    if (IS_FIXNUM(v)) {
        fprintf(out, "%lld", (long long)UNFIX(v));
    } else if ((v & 0xFF) == CHAR_TAG) {
        print_char(out, (uint32_t)(v >> CHAR_SHIFT), write);
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

// ───────────────────────────── symbols ─────────────────────────────
//
// Symbols are compared with eq?, so each name must map to exactly one object.
// The compiler emits the program's symbols as static data; symbols made at
// run time are malloc'd (outside the GC heap, never freed). An open-addressing
// hash table maps names to symbols.

static value *sym_table;
static size_t sym_cap, sym_count;

static uint64_t hash_bytes(const char *s, size_t n) {
    uint64_t h = 1469598103934665603ULL;   // FNV-1a
    for (size_t i = 0; i < n; i++) h = (h ^ (unsigned char)s[i]) * 1099511628211ULL;
    return h;
}

static void sym_insert(value sym) {
    value *p = FIELDS(sym);
    size_t i = hash_bytes((const char *)(p + 1), UNFIX(p[0])) & (sym_cap - 1);
    while (sym_table[i]) i = (i + 1) & (sym_cap - 1);
    sym_table[i] = sym;
    sym_count++;
}

static void sym_grow(void) {
    value *old = sym_table;
    size_t old_cap = sym_cap;
    sym_cap = sym_cap ? sym_cap * 2 : 256;
    sym_table = calloc(sym_cap, sizeof(value));
    if (!sym_table) { fprintf(stderr, "out of memory\n"); exit(1); }
    sym_count = 0;
    for (size_t i = 0; i < old_cap; i++) if (old[i]) sym_insert(old[i]);
    free(old);
}

value scheme_string_to_symbol(value str) {
    if (!sym_table) {
        sym_grow();
        for (value *s = scm_symbols_start; s < scm_symbols_end; s++) {
            if (sym_count * 2 >= sym_cap) sym_grow();
            sym_insert(*s);
        }
    }
    value *sp = FIELDS(str);
    size_t len = UNFIX(sp[0]);
    const char *bytes = (const char *)(sp + 1);
    size_t i = hash_bytes(bytes, len) & (sym_cap - 1);
    for (; sym_table[i]; i = (i + 1) & (sym_cap - 1)) {
        value *p = FIELDS(sym_table[i]);
        if ((size_t)UNFIX(p[0]) == len && memcmp(p + 1, bytes, len) == 0) return sym_table[i];
    }
    value *obj = malloc(round16(8 + len));
    if (!obj) { fprintf(stderr, "out of memory\n"); exit(1); }
    obj[0] = sp[0];
    memcpy(obj + 1, bytes, len);
    value sym = (value)obj + SYMBOL_TAG;
    if (sym_count * 2 >= sym_cap) sym_grow();
    sym_insert(sym);
    return sym;
}

void scheme_exit(int64_t code) {
    exit((int)code);
}

// ───────────────────────────── stack ─────────────────────────────
//
// Scheme code runs on its own large stack (default 1 GiB of address space,
// only touched pages use memory) with an inaccessible guard region at the
// bottom, so deep recursion either works or fails with a clear message.

#define GUARD_SIZE ((size_t)64 << 20)
static char *stack_low;

static void on_fault(int sig, siginfo_t *info, void *ctx) {
    (void)ctx;
    char *addr = info->si_addr;
    const char *msg = (addr >= stack_low && addr < stack_low + GUARD_SIZE)
        ? "error: stack overflow (recursion too deep; set QUILL_STACK_MB for a bigger stack)\n"
        : (sig == SIGSEGV ? "error: segmentation fault\n" : "error: bus error\n");
    fflush(stdout);
    write(2, msg, strlen(msg));
    _exit(1);
}

static char *make_stack(size_t size) {
    char *p = mmap(NULL, size + GUARD_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (p == MAP_FAILED) { fprintf(stderr, "could not allocate a %zu MiB stack\n", size >> 20); exit(1); }
    mprotect(p, GUARD_SIZE, PROT_NONE);
    stack_low = p;

    // the handler needs a stack of its own, since ours just overflowed
    stack_t alt = { .ss_sp = malloc(1 << 16), .ss_size = 1 << 16, .ss_flags = 0 };
    sigaltstack(&alt, NULL);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_fault;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    return p + GUARD_SIZE + size;   // stacks grow down; the top is 16-byte aligned
}

// ───────────────────────────── main ─────────────────────────────

int main(void) {
    // start small and let the GC grow the heap; QUILL_HEAP_KB overrides
    size_t size = 256 * 1024;
    const char *env = getenv("QUILL_HEAP_KB");
    if (env && atol(env) > 0) size = round16((size_t)atol(env) * 1024);
    heap_start = malloc(size);
    if (!heap_start) { fprintf(stderr, "could not allocate heap\n"); return 1; }
    heap_end = heap_start + size;

    size_t stack_mb = 1024;
    const char *senv = getenv("QUILL_STACK_MB");
    if (senv && atol(senv) > 0) stack_mb = (size_t)atol(senv);
    char *stack_top = make_stack(stack_mb << 20);

    value result = scheme_entry(heap_start, heap_end, stack_top);
    if (result != VOID_V) {
        print(stdout, result, 1);
        putchar('\n');
    }
    if (getenv("QUILL_GC_STATS"))
        fprintf(stderr, "gc: %zu collections, final heap %zu KiB\n", gc_count,
                (size_t)(heap_end - heap_start) / 1024);
    return 0;
}
