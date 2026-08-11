// A process-wide zeroing allocator, for testing whether a behaviour depends on reading
// uninitialized heap memory.
//
// Allocators differ in what they leave in freshly allocated memory, and some zero it. A Rust
// #[global_allocator] can only emulate that for allocations made by Rust; anything the system audio
// frameworks or libcubeb's C++ allocate goes through libmalloc. Interposing malloc reaches all of
// them.
//
// Load it with DYLD_INSERT_LIBRARIES. Two caveats. It cannot affect coreaudiod, so only the
// in-process half of the audio stack is covered. And it does not make uninitialized *stack* reads
// go away, which is the other common shape of this kind of dependency.
//
// Set CUBEB_ZEROING_MALLOC_VERBOSE=1 to confirm it is actually loaded.

#include <malloc/malloc.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static _Atomic unsigned long long g_allocations;

// calloc is deliberately not interposed, so calling it here both zeroes and avoids recursing
// through the interposed malloc.
static void *zeroing_malloc(size_t size)
{
    g_allocations++;
    return calloc(1, size);
}

static void *zeroing_realloc(void *ptr, size_t size)
{
    // Grown regions are uninitialized past the old contents, so zero the tail ourselves.
    size_t old_size = ptr ? malloc_size(ptr) : 0;
    void *result = realloc(ptr, size);
    if (result && size > old_size) {
        memset((char *)result + old_size, 0, size - old_size);
    }
    g_allocations++;
    return result;
}

static void *zeroing_valloc(size_t size)
{
    void *result = valloc(size);
    if (result) {
        memset(result, 0, malloc_size(result));
    }
    g_allocations++;
    return result;
}

static void report(void)
{
    const char *verbose = getenv("CUBEB_ZEROING_MALLOC_VERBOSE");
    if (verbose && verbose[0] == '1') {
        fprintf(stderr, "zeroing malloc: %llu allocations went through the interposer\n",
                g_allocations);
    }
}

__attribute__((constructor)) static void init(void)
{
    const char *verbose = getenv("CUBEB_ZEROING_MALLOC_VERBOSE");
    if (verbose && verbose[0] == '1') {
        fprintf(stderr, "zeroing malloc: interposer loaded\n");
    }
    atexit(report);
}

// dyld interposition table: each entry is {replacement, original}.
#define INTERPOSE(replacement, original)                                                           \
    __attribute__((used, section("__DATA,__interpose"))) static struct {                           \
        const void *replacement;                                                                   \
        const void *original;                                                                      \
    } interpose_##original = {(const void *)(unsigned long)&replacement,                           \
                              (const void *)(unsigned long)&original};

INTERPOSE(zeroing_malloc, malloc)
INTERPOSE(zeroing_realloc, realloc)
INTERPOSE(zeroing_valloc, valloc)
