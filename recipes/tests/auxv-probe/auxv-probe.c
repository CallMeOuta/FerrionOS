#define _GNU_SOURCE
#include <elf.h>
#include <errno.h>
#include <link.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <unistd.h>
#ifdef __x86_64__
#include <cpuid.h>
#endif

#ifndef AT_SECURE
#define AT_SECURE 23
#endif

enum {
    AUXV_KEY_MAX = 51,
    AUXV_RANDOM_LEN = 16,
};

extern const ElfW(Ehdr) __ehdr_start;
extern void _start(void);

static int failures = 0;

static unsigned long read_auxv(unsigned long key, int *out_errno) {
    errno = 0;
    unsigned long val = getauxval(key);
    *out_errno = errno;
    return val;
}

static const char *errno_label(int err, char *buf, size_t size) {
    if (err == 0) {
        return "0";
    }
    if (err == ENOENT) {
        return "ENOENT";
    }
    snprintf(buf, size, "%d", err);
    return buf;
}

static void record_check(const char *name, int ok, unsigned long got, unsigned long want, int err) {
    char errbuf[16];
    if (!ok) {
        failures++;
    }
    printf("check %s %s got=0x%lx want=0x%lx errno=%s\n",
           name, ok ? "PASS" : "FAIL", got, want, errno_label(err, errbuf, sizeof(errbuf)));
}

#ifdef AUXV_DYNAMIC
struct dl_main_phdr {
    int found;
    uintptr_t phdr;
    unsigned long phnum;
};

static int dl_first_cb(struct dl_phdr_info *info, size_t size, void *data) {
    struct dl_main_phdr *st = (struct dl_main_phdr *)data;
    (void)size;
    if (!st->found) {
        st->found = 1;
        st->phdr = (uintptr_t)info->dlpi_phdr;
        st->phnum = (unsigned long)info->dlpi_phnum;
    }
    return 1;
}
#endif

int main(int argc, char **argv) {
    char errbuf[16];
    for (unsigned long k = 0; k <= AUXV_KEY_MAX; k++) {
        int err = 0;
        unsigned long val = read_auxv(k, &err);
        printf("auxv %lu 0x%lx %s\n", k, val, errno_label(err, errbuf, sizeof(errbuf)));
    }

    int err = 0;
    unsigned long val = 0;

    long sc_pagesz = sysconf(_SC_PAGESIZE);
    val = read_auxv(AT_PAGESZ, &err);
    record_check("pagesz", err == 0 && sc_pagesz > 0 && val == (unsigned long)sc_pagesz,
                 val, sc_pagesz > 0 ? (unsigned long)sc_pagesz : 0UL, err);

    long sc_clktck = sysconf(_SC_CLK_TCK);
    val = read_auxv(AT_CLKTCK, &err);
    record_check("clktck", err == 0 && sc_clktck > 0 && val == (unsigned long)sc_clktck,
                 val, sc_clktck > 0 ? (unsigned long)sc_clktck : 0UL, err);

    val = read_auxv(AT_PHENT, &err);
    record_check("phent", err == 0 && val == (unsigned long)sizeof(ElfW(Phdr)),
                 val, (unsigned long)sizeof(ElfW(Phdr)), err);

    unsigned long expect_phdr = (unsigned long)((const char *)&__ehdr_start + __ehdr_start.e_phoff);
    unsigned long at_phdr = read_auxv(AT_PHDR, &err);
    record_check("ehdr_phdr", err == 0 && at_phdr == expect_phdr, at_phdr, expect_phdr, err);

    unsigned long expect_phnum = (unsigned long)__ehdr_start.e_phnum;
    unsigned long at_phnum = read_auxv(AT_PHNUM, &err);
    record_check("ehdr_phnum", err == 0 && at_phnum == expect_phnum, at_phnum, expect_phnum, err);

    unsigned long expect_entry = (unsigned long)(uintptr_t)&_start;
    val = read_auxv(AT_ENTRY, &err);
    record_check("entry", err == 0 && val == expect_entry, val, expect_entry, err);

#ifdef AUXV_DYNAMIC
    struct dl_main_phdr dl_main = {0, 0, 0};
    dl_iterate_phdr(dl_first_cb, &dl_main);
    record_check("dl_phdr", dl_main.found && at_phdr == (unsigned long)dl_main.phdr,
                 at_phdr, (unsigned long)dl_main.phdr, 0);
    record_check("dl_phnum", dl_main.found && at_phnum == dl_main.phnum,
                 at_phnum, dl_main.phnum, 0);
#endif

    val = read_auxv(AT_BASE, &err);
#ifdef AUXV_STATIC
    record_check("base", err == 0 && val == 0, val, 0UL, err);
#else
    int base_elf = (err == 0 && val != 0 && memcmp((const void *)(uintptr_t)val, "\x7f""ELF", 4) == 0);
    record_check("base", base_elf, val, 1UL, err);
#endif

    val = read_auxv(AT_FLAGS, &err);
    record_check("flags", err == 0 && val == 0, val, 0UL, err);

    unsigned long uid = (unsigned long)getuid();
    unsigned long euid = (unsigned long)geteuid();
    unsigned long gid = (unsigned long)getgid();
    unsigned long egid = (unsigned long)getegid();

    val = read_auxv(AT_UID, &err);
    record_check("uid", err == 0 && val == uid, val, uid, err);
    val = read_auxv(AT_EUID, &err);
    record_check("euid", err == 0 && val == euid, val, euid, err);
    val = read_auxv(AT_GID, &err);
    record_check("gid", err == 0 && val == gid, val, gid, err);
    val = read_auxv(AT_EGID, &err);
    record_check("egid", err == 0 && val == egid, val, egid, err);

    unsigned long expect_secure = (uid != euid || gid != egid) ? 1UL : 0UL;
    val = read_auxv(AT_SECURE, &err);
    record_check("secure", err == 0 && val == expect_secure, val, expect_secure, err);

#ifdef __x86_64__
    unsigned int eax = 0, ebx = 0, ecx = 0, edx = 0;
    int cpuid_ok = __get_cpuid(1, &eax, &ebx, &ecx, &edx);
    val = read_auxv(AT_HWCAP, &err);
#ifdef __redox__
    record_check("hwcap", err == 0 && cpuid_ok && val != 0 && val == (unsigned long)edx,
                 val, (unsigned long)edx, err);
#else
    record_check("hwcap", err == 0 && cpuid_ok && val != 0, val, (unsigned long)edx, err);
#endif
#endif

    val = read_auxv(AT_RANDOM, &err);
    int random_ok = 0;
    if (err == 0 && val != 0) {
        const unsigned char *rnd = (const unsigned char *)(uintptr_t)val;
        for (int i = 0; i < AUXV_RANDOM_LEN; i++) {
            if (rnd[i] != 0) {
                random_ok = 1;
                break;
            }
        }
    }
    record_check("random", random_ok, val, 1UL, err);

    val = read_auxv(AT_EXECFN, &err);
    const char *expect_execfn = (argc >= 2 && argv[1] != NULL) ? argv[1] : (argc >= 1 ? argv[0] : "");
    const char *got_execfn = (err == 0 && val != 0) ? (const char *)(uintptr_t)val : "";
    int execfn_ok = (err == 0 && val != 0 && got_execfn[0] != '\0' && strcmp(got_execfn, expect_execfn) == 0);
    if (!execfn_ok) {
        failures++;
    }
    printf("check execfn %s got=%s want=%s errno=%s\n",
           execfn_ok ? "PASS" : "FAIL", got_execfn, expect_execfn, errno_label(err, errbuf, sizeof(errbuf)));

    if (failures != 0) {
        printf("auxv-probe: FAIL %d\n", failures);
        return 1;
    }
    printf("auxv-probe: DONE\n");
    return 0;
}
