#include <elf.h>
#include <link.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

extern int relro_helper_data;
extern int relro_helper_value(void);
extern uintptr_t relro_helper_self_addr(void);
extern uintptr_t relro_helper_relro_slot(void);

int *const main_relro_ptr = &relro_helper_data;
static volatile int mutable_data = 1;

enum {
    MAX_PHDRS = 128,
    MAX_DYN_ENTRIES = 1024,
    MAX_JMPREL_ENTRIES = 4096,
};

struct self_info {
    int found;
    uintptr_t base;
    uintptr_t load_start;
    uintptr_t load_end;
    uintptr_t relro_start;
    uintptr_t relro_end;
    int has_relro;
    int bind_now;
    uintptr_t got_slot;
};

static int find_self_cb(struct dl_phdr_info *info, size_t size, void *data)
{
    struct self_info *out = (struct self_info *)data;
    uintptr_t main_addr = (uintptr_t)&find_self_cb;
    uintptr_t min_v = UINTPTR_MAX;
    uintptr_t max_v = 0;
    const Elf64_Dyn *dyn = NULL;
    size_t ph_count;
    int covers_self = 0;

    (void)size;
    if (out->found || info->dlpi_phdr == NULL) {
        return 0;
    }

    ph_count = info->dlpi_phnum;
    if (ph_count > MAX_PHDRS) {
        ph_count = MAX_PHDRS;
    }

    for (size_t i = 0; i < ph_count; i++) {
        const Elf64_Phdr *ph = &info->dlpi_phdr[i];
        if (ph->p_type == PT_LOAD) {
            uintptr_t s = info->dlpi_addr + (uintptr_t)ph->p_vaddr;
            uintptr_t e = s + (uintptr_t)ph->p_memsz;
            if (s < min_v) {
                min_v = s;
            }
            if (e > max_v) {
                max_v = e;
            }
            if (main_addr >= s && main_addr < e) {
                covers_self = 1;
            }
        }
    }

    if (!covers_self) {
        return 0;
    }

    out->found = 1;
    out->base = info->dlpi_addr;
    out->load_start = min_v;
    out->load_end = max_v;

    for (size_t i = 0; i < ph_count; i++) {
        const Elf64_Phdr *ph = &info->dlpi_phdr[i];
        if (ph->p_type == PT_GNU_RELRO) {
            out->has_relro = 1;
            out->relro_start = info->dlpi_addr + (uintptr_t)ph->p_vaddr;
            out->relro_end = out->relro_start + (uintptr_t)ph->p_memsz;
        } else if (ph->p_type == PT_DYNAMIC) {
            dyn = (const Elf64_Dyn *)(info->dlpi_addr + (uintptr_t)ph->p_vaddr);
        }
    }

    if (dyn == NULL) {
        return 1;
    }

    uintptr_t jmprel = 0;
    size_t pltrelsz = 0;
    const Elf64_Sym *symtab = NULL;
    const char *strtab = NULL;
    size_t strsz = 0;

    for (size_t i = 0; i < MAX_DYN_ENTRIES; i++) {
        Elf64_Sxword tag = dyn[i].d_tag;
        uintptr_t val = (uintptr_t)dyn[i].d_un.d_val;
        if (tag == DT_NULL) {
            break;
        } else if (tag == DT_BIND_NOW) {
            out->bind_now = 1;
        } else if (tag == DT_FLAGS && (val & DF_BIND_NOW)) {
            out->bind_now = 1;
        } else if (tag == DT_FLAGS_1 && (val & DF_1_NOW)) {
            out->bind_now = 1;
        } else if (tag == DT_JMPREL) {
            jmprel = val;
        } else if (tag == DT_PLTRELSZ) {
            pltrelsz = (size_t)val;
        } else if (tag == DT_SYMTAB) {
            symtab = (const Elf64_Sym *)val;
        } else if (tag == DT_STRTAB) {
            strtab = (const char *)val;
        } else if (tag == DT_STRSZ) {
            strsz = (size_t)val;
        }
    }

    if (jmprel != 0 && symtab != NULL && strtab != NULL && strsz > 0) {
        uintptr_t jmprel_addr = jmprel < info->dlpi_addr ? info->dlpi_addr + jmprel : jmprel;
        const Elf64_Sym *syms = (uintptr_t)symtab < info->dlpi_addr
            ? (const Elf64_Sym *)(info->dlpi_addr + (uintptr_t)symtab)
            : symtab;
        const char *strs = (uintptr_t)strtab < info->dlpi_addr
            ? (const char *)(info->dlpi_addr + (uintptr_t)strtab)
            : strtab;
        size_t count = pltrelsz / sizeof(Elf64_Rela);
        if (count > MAX_JMPREL_ENTRIES) {
            count = MAX_JMPREL_ENTRIES;
        }
        const Elf64_Rela *relas = (const Elf64_Rela *)jmprel_addr;
        for (size_t i = 0; i < count; i++) {
            uint32_t sym_idx = (uint32_t)(relas[i].r_info >> 32);
            if (sym_idx == 0) {
                continue;
            }
            uint32_t name_off = syms[sym_idx].st_name;
            if ((size_t)name_off < strsz &&
                strcmp(strs + name_off, "relro_helper_value") == 0) {
                out->got_slot = info->dlpi_addr + (uintptr_t)relas[i].r_offset;
                break;
            }
        }
    }

    return 1;
}

int main(int argc, char **argv)
{
    struct self_info info;
    memset(&info, 0, sizeof(info));
    dl_iterate_phdr(find_self_cb, &info);

    if (!info.found) {
        fputs("RELRO_ERROR self_not_found\n", stderr);
        return 2;
    }

    if (argc != 2) {
        fputs("Usage: relro-probe info|write-got|write-relro|write-dso\n", stderr);
        return 2;
    }

    long page_size_l = sysconf(_SC_PAGESIZE);
    uintptr_t page_size = page_size_l > 0 ? (uintptr_t)page_size_l : 4096;
    uintptr_t relro_page_start = info.relro_start & ~(page_size - 1);
    uintptr_t relro_page_end = info.relro_end & ~(page_size - 1);

    if (strcmp(argv[1], "info") == 0) {
        if (info.got_slot == 0) {
            fputs("RELRO_ERROR got_slot_not_found\n", stderr);
            return 3;
        }
        uintptr_t before_val = *(volatile uintptr_t *)info.got_slot;
        int bound_before_call = !(before_val >= info.load_start && before_val < info.load_end);
        int value = relro_helper_value();
        uintptr_t after_val = *(volatile uintptr_t *)info.got_slot;
        uintptr_t target = relro_helper_self_addr();
        int slot_in_relro = info.has_relro &&
            info.got_slot >= relro_page_start &&
            info.got_slot < relro_page_end;
        int ptr_in_relro = info.has_relro &&
            (uintptr_t)&main_relro_ptr >= relro_page_start &&
            (uintptr_t)&main_relro_ptr < relro_page_end;
        mutable_data = 99;
        printf(
            "RELRO_INFO bind_now=%d has_relro=%d slot_in_relro=%d ptr_in_relro=%d "
            "bound_before_call=%d value=%d match=%d mutable=%d\n",
            info.bind_now,
            info.has_relro,
            slot_in_relro,
            ptr_in_relro,
            bound_before_call,
            value,
            after_val == target,
            (int)mutable_data
        );
        fflush(stdout);
        return 0;
    }

    if (strcmp(argv[1], "write-got") == 0) {
        if (info.got_slot == 0) {
            fputs("RELRO_ERROR got_slot_not_found\n", stderr);
            return 3;
        }
        int value = relro_helper_value();
        if (value != 42) {
            fputs("RELRO_ERROR unexpected_helper_value\n", stderr);
            return 4;
        }
        puts("RELRO_BEFORE_WRITE");
        fflush(stdout);
        *(volatile uintptr_t *)info.got_slot = 0x1234;
        puts("RELRO_WRITE_SUCCEEDED");
        fflush(stdout);
        return 0;
    }

    if (strcmp(argv[1], "write-relro") == 0) {
        if (*main_relro_ptr != 42) {
            fputs("RELRO_ERROR unexpected_relro_ptr\n", stderr);
            return 4;
        }
        puts("RELRO_BEFORE_WRITE");
        fflush(stdout);
        *(volatile uintptr_t *)(uintptr_t)&main_relro_ptr = 0x1234;
        puts("RELRO_WRITE_SUCCEEDED");
        fflush(stdout);
        return 0;
    }

    if (strcmp(argv[1], "write-dso") == 0) {
        uintptr_t slot = relro_helper_relro_slot();
        if (slot == 0) {
            fputs("RELRO_ERROR dso_slot_zero\n", stderr);
            return 3;
        }
        puts("RELRO_BEFORE_WRITE");
        fflush(stdout);
        *(volatile uintptr_t *)slot = 0x1234;
        puts("RELRO_WRITE_SUCCEEDED");
        fflush(stdout);
        return 0;
    }

    fputs("RELRO_ERROR unknown_mode\n", stderr);
    return 2;
}
