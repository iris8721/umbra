#include "../umbra.h"
#include <stdio.h>
#include <stdlib.h>

static int fn_print(umbra_State *U) {
    int n = umbra_gettop(U);
    for (int i = 1; i <= n; i++) {
        if (i > 1) putchar('\t');
        switch (umbra_type(U, i)) {
            case 0: fputs("none",  stdout); break;
            case 1: fputs(umbra_toboolean(U, i) ? "true" : "false", stdout); break;
            case 2: printf("%lld", (long long)umbra_tointeger(U, i)); break;
            case 3: printf("%g",   umbra_tonumber(U, i)); break;
            default: {
                const char *s = umbra_tostring(U, i);
                fputs(s ? s : "?", stdout);
                break;
            }
        }
    }
    putchar('\n');
    return 0;
}

static char *read_file(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return NULL; }
    long sz = ftell(f);
    if (sz < 0) { fclose(f); return NULL; }
    rewind(f);
    char *buf = malloc((size_t)sz + 1);
    if (!buf) { fclose(f); return NULL; }
    size_t n = fread(buf, 1, (size_t)sz, f);
    buf[n] = '\0';
    fclose(f);
    return buf;
}

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "word_count.umbra";

    char *src = read_file(path);
    if (!src) {
        fprintf(stderr, "error: could not read '%s'\n", path);
        return 1;
    }

    umbra_State *U = umbra_newstate();
    umbra_register(U, "print", fn_print);

    if (umbra_dostring(U, src) != UMBRA_OK) {
        const char *msg = umbra_tostring(U, -1);
        fprintf(stderr, "error: %s\n", msg ? msg : "(no message)");
        free(src);
        umbra_close(U);
        return 1;
    }

    free(src);
    umbra_close(U);
    return 0;
}
