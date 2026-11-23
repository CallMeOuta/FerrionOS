#!/usr/bin/env python3
import argparse
import json
import re
import sys
import tarfile
from collections import Counter, defaultdict
from pathlib import Path

MAX_FILES = 80_000
MAX_FILE_BYTES = 48 << 20
MAX_TAR_MEMBERS = 250_000
MAX_LISTED = 4000
TOP_NAMES = 8

STRIP = re.compile(r'/\*.*?\*/|//[^\n]*|"(?:\\.|[^"\\\n])*"|\'(?:\\.|[^\'\\\n])+\'', re.S)
INCLUDE = re.compile(r'^[ \t]*#[ \t]*include[ \t]*<([^>\n]+)>', re.M)
DEFINE = re.compile(r'^[ \t]*#[ \t]*define[ \t]+([A-Za-z_]\w*)(\()?', re.M)
PREPROC = re.compile(r'^[ \t]*#[^\n]*', re.M)
PORT_CONDITIONAL = re.compile(
    r'^[ \t]*#[ \t]*(?:if|ifdef|ifndef|elif)\b[^\n]*(?:__FreeBSD__|__OpenBSD__|__linux__|\bnotyet\b|__notyet__|FREEBSD_NOTYET)', re.M)
NEVER_DEFINED = ('notyet', '__notyet__', 'FREEBSD_NOTYET')
SCAN = re.compile(r'[{}]|(?<![\w.>])([A-Za-z_]\w*)[ \t\n]*\((?![ \t\n]*\*)')
CALL = re.compile(r'(?<![\w.>])([A-Za-z_]\w*)[ \t\n]*\((?![ \t\n]*\*)')
IDENT = re.compile(r'(?<![\w.>])([A-Za-z_]\w*)')
REF = re.compile(r'(?:[=(,&?:]|\breturn)[ \t\n]*([A-Za-z_]\w*)[ \t\n]*(?=[,);}])')
DECLARED = re.compile(r'(\w+|\*)[ \t]+\**([A-Za-z_]\w*)[ \t]*(?=[;,=)\[])')
STRUCT_USE = re.compile(r'\b(?:struct|union)[ \t\n]+([A-Za-z_]\w*)')
STRUCT_DEF = re.compile(r'\b(?:struct|union)[ \t\n]+([A-Za-z_]\w*)[ \t\n]*\{')
ENUM = re.compile(r'\benum\b[^{;()]*\{([^}]*)\}')
ENUMERATOR = re.compile(r'(?:^|,)[ \t\n]*([A-Za-z_]\w*)')
EXPORT = re.compile(r'\bEXPORT_SYMBOL(?:_GPL|_NS|_NS_GPL|_FOR_MODULES|_FOR_KVM)?[ \t]*\([ \t]*([A-Za-z_]\w*)')
SPDX = re.compile(r'SPDX-License-Identifier:[ \t]*([^\n*]+)')
SRCS = re.compile(r'\b([\w.+-]+\.c)\b')

KEYWORDS = frozenset(
    'if while for switch return sizeof typeof __typeof__ __typeof defined do else case goto '
    '__attribute__ __attribute _Static_assert alignof __alignof__ _Alignof offsetof __builtin_offsetof '
    'int char long short unsigned signed void float double bool struct union enum const static inline '
    'extern volatile register typedef asm __asm__ __extension__ _Generic __builtin_types_compatible_p '
    '__builtin_choose_expr __builtin_constant_p __builtin_expect'.split()
)
NOT_A_TYPE = frozenset('return else goto case do'.split())

SET_HEADERS = (
    'include/drm/',
    'include/uapi/drm/',
    'include/linux/dma-buf.h',
    'include/linux/dma-fence.h',
    'include/linux/dma-fence-array.h',
    'include/linux/dma-fence-chain.h',
    'include/linux/dma-fence-unwrap.h',
    'include/linux/dma-resv.h',
    'include/linux/sync_file.h',
    'include/uapi/linux/dma-buf.h',
    'include/uapi/linux/sync_file.h',
    'include/trace/events/dma_fence.h',
    'include/trace/events/sync_trace.h',
    'include/trace/events/gpu_mem.h',
)
DMABUF_FILES = frozenset(
    'dma-buf.c dma-fence.c dma-fence-array.c dma-fence-chain.c dma-fence-unwrap.c dma-resv.c '
    'sync_file.c sync_file.h sync_debug.h sync_trace.h dma-buf-sysfs-stats.h'.split()
)
SCOPES = ('amdgpu', 'nouveau', 'drm', 'ttm', 'sched', 'dmabuf', 'virtio')
STACKS = {
    'amdgpu': ('amdgpu', 'drm', 'ttm', 'sched', 'dmabuf'),
    'nouveau': ('nouveau', 'drm', 'ttm', 'sched', 'dmabuf'),
    'virtio': ('virtio', 'drm', 'dmabuf'),
}
UNFILTERED_SCOPES = frozenset(('virtio', 'nouveau'))
STACK_EXTRA = {
    'virtio': frozenset(('drm_gem_shmem_helper.c',)),
    'nouveau': frozenset(('drm_gpuvm.c', 'drm_exec.c')),
}
STACK_OPTIONS = {
    'virtio': frozenset(('DRM_VIRTIO_GPU', 'DRM_VIRTIO_GPU_KMS', 'VIRTIO', 'VIRTIO_PCI', 'DRM_GEM_SHMEM_HELPER', 'MMU')),
    'nouveau': frozenset(('DRM_NOUVEAU', 'DRM_NOUVEAU_BACKLIGHT', 'NOUVEAU_DEBUG', 'NOUVEAU_DEBUG_DEFAULT', 'IOMMU_API',
                          'FW_LOADER', 'I2C', 'POWER_SUPPLY', 'THERMAL', 'ACPI_VIDEO', 'INPUT', 'HWMON', 'MMU',
                          'DRM_EXEC', 'DRM_GPUVM', 'DRM_SCHED', 'DRM_TTM')),
}
AMD_EXCLUDED = ('drivers/gpu/drm/amd/amdkfd/', 'drivers/gpu/drm/amd/include/asic_reg/')
LAYOUTS = {
    'linux': (('drivers/gpu/drm', 'drivers/gpu/drm'), ('drivers/dma-buf', 'drivers/dma-buf'),
              ('include/drm', 'include/drm'), ('include/uapi/drm', 'include/uapi/drm'),
              ('include/linux', 'include/linux'), ('include/uapi/linux', 'include/uapi/linux'),
              ('include/trace/events', 'include/trace/events')),
    'openbsd': (('drivers/gpu/drm', 'sys/dev/pci/drm'), ('include/drm', 'sys/dev/pci/drm/include/drm'),
                ('include/uapi/drm', 'sys/dev/pci/drm/include/uapi/drm'),
                ('include/linux', 'sys/dev/pci/drm/include/linux'),
                ('include/uapi/linux', 'sys/dev/pci/drm/include/uapi/linux')),
}


GROUPS = (
    ('firmware', 'Firmware loading', r'^linux/firmware\.h$'),
    ('dma', 'DMA mapping and IOMMU',
     r'^linux/(dma-|scatterlist|iommu|swiotlb|iova|dmapool|iosys-map|agp_backend|agpgart|pci-p2pdma)|^asm/(agp|iommu)'),
    ('pci', 'PCI and device registers',
     r'^linux/(pci|io\.h|ioport|iopoll|aperture|vgaarb|vga_switcheroo|io-64-nonatomic|msi|aer|resource)|^asm(-generic)?/(io\.h|iomap|pci|early_ioremap)|^uapi/linux/pci|^video/vga\.h'),
    ('irq', 'Interrupts',
     r'^linux/(interrupt|irq|hardirq|bottom_half|preempt)|^asm(-generic)?/(irq|hardirq|hw_irq|preempt|apic)'),
    ('time', 'Time, timers and delays',
     r'^linux/(timer|hrtimer|jiffies|ktime|timekeeping|delay|time|clocksource|tick|sched/clock)|^asm(-generic)?/(timex|delay|tsc)|^vdso/(jiffies|ktime|time)|^uapi/linux/time'),
    ('work', 'Work queues, threads and waiting',
     r'^linux/(workqueue|kthread|completion|wait|swait|sched|async|freezer|smp|cpu\.h|cpuhotplug|task_work|signal|pid|rcuwait)|^asm(-generic)?/(current|smp|thread_info)|^uapi/linux/sched'),
    ('lock', 'Locks, atomics and RCU',
     r'^linux/(spinlock|mutex|rwsem|rwlock|seqlock|ww_mutex|lockdep|atomic|refcount|kref|rcu|srcu|percpu-rwsem|local_lock|bit_spinlock|semaphore|rtmutex|cleanup|lockref|debug_locks)|^asm(-generic)?/(atomic|cmpxchg|barrier|spinlock|rwonce|qspinlock|qrwlock|local)'),
    ('alloc', 'Memory allocation',
     r'^linux/(slab|vmalloc|gfp|mempool|kmemleak|percpu|alloc_tag|kasan|kfence|kmsan)'),
    ('mm', 'Pages, mappings and user memory',
     r'^linux/(mm|page|highmem|pagemap|shmem_fs|swap|mman|pfn|hugetlb|mmu_notifier|hmm|set_memory|io-mapping|memremap|shrinker|pagevec|mmzone|migrate|vmstat|oom|mmap_lock|memory|pgtable|huge_mm|rmap|memcontrol|numa|topology|cacheflush)|^asm(-generic)?/(page|pgtable|pgalloc|tlb|cacheflush|set_memory|memtype|pat|mmu|fixmap|highmem|topology|getorder|mem_encrypt)|^uapi/(linux|asm-generic|asm)/mman'),
    ('file', 'Files, user access and ioctl plumbing',
     r'^linux/(fs|file|fdtable|uaccess|poll|anon_inodes|seq_file|seq_buf|mount|pseudo_fs|dcache|uio|compat|capability|cred|security|eventfd|eventpoll|fcntl|namei|path|proc_fs|nospec|ioctl|syscalls|uidgid|utsname|stat|kdev_t|cdev|miscdevice|blkdev|nfs|magic)|^asm(-generic)?/(uaccess|ioctl|compat|unistd)|^uapi/(linux|asm-generic|asm)/(fs|ioctl|fcntl|stat|poll|magic)'),
    ('data', 'Data structures',
     r'^linux/(list|hashtable|hash\.h|jhash|rbtree|interval_tree|xarray|idr|radix-tree|bitmap|bitops|bits|bitfield|kfifo|llist|sort|bsearch|circ_buf|maple_tree|cpumask|nodemask|find|stackdepot|wordpart)|^asm(-generic)?/(bitops|bitsperlong)|^vdso/bits'),
    ('pm', 'Power management, hwmon and thermal',
     r'^linux/(pm|suspend|thermal|hwmon|power_supply|cpufreq|reboot|notifier|kmsg_dump|units|power/)'),
    ('acpi', 'ACPI, firmware tables and platform devices',
     r'^linux/(acpi|dmi|efi|platform_device|apple-gmux|mfd/|mod_devicetable|platform_data/)|^acpi/|^asm/(acpi|efi)|^xen/'),
    ('arch', 'CPU features and floating point in the driver',
     r'^linux/(cpufeature|cc_platform|processor|mem_encrypt|fpu|kdebug|jump_label|static_call|static_key)|^asm/(fpu|cpufeature|cpu_device_id|processor|msr|mtrr|special_insns|cpu|intel-family|hypervisor|mce|neon|switch_to|kvm|nospec-branch|alternative|vdso/processor)'),
    ('console', 'Console, framebuffer emulation and backlight',
     r'^linux/(fb|font|console|vt|sysrq|backlight|kd\.h|tty|linux_logo|kgdb|panic|screen_info|sysfb)|^video/|^uapi/linux/(fb|kd|vt)'),
    ('bus', 'I2C, HDMI, component and audio links',
     r'^linux/(i2c|hdmi|component|cec|pwm|gpio|of|property|fwnode|regulator|clk|media-bus-format|uuid|phy|mux)|^sound/|^media/|^dt-bindings/'),
    ('virtio', 'The virtio transport',
     r'^linux/(virtio|vringh|virtio_)|^uapi/linux/virtio'),
    ('devmodel', 'Device model, modules and sysfs',
     r'^linux/(device|kobject|sysfs|module|moduleparam|init|export|kconfig|kmod|kernfs|sysctl|uevent)|^asm(-generic)?/(module|export)'),
    ('trace', 'Tracing, debugfs and debugging aids',
     r'^linux/(tracepoint|trace|ftrace|debugfs|stacktrace|fault-inject|dynamic_debug|kallsyms|perf_event|relay|ref_tracker|debugobjects|devcoredump|kcov|coredump)|^trace/|^kunit/|^asm(-generic)?/(ftrace|trace)'),
    ('util', 'Kernel utilities: strings, maths, errors, logging',
     r'^linux/(kernel|string|math|minmax|log2|gcd|lcm|overflow|limits|types|err\.h|errno|errname|printk|dev_printk|ratelimit|random|crc|byteorder|unaligned|ctype|kstrtox|sprintf|hex|align|array_size|container_of|stddef|stdarg|typecheck|args|util_macros|sizes|bcd|fortify-string|compiler|bug|build_bug|once|stringify|const|linkage|cache|average|zlib|instrumented|nls)|^asm(-generic)?/(bug|byteorder|unaligned|div64|string|cache|linkage|swab|errno|types)|^uapi/(linux|asm-generic|asm)/(types|kernel|const|swab|byteorder|errno|stddef|kfd_ioctl)|^vdso/|^generated/'),
)
NOT_FOR_A_GPU = r'^net/|^crypto/|^linux/(ieee80211|nl80211|netdev|skbuff|etherdevice|ethtool|if_|in\.h|in6|ip\.h|ipv6|tcp|udp|socket|usb|mhi|rfkill|wireless|inetdevice|netlink|rtnetlink|mii|bluetooth|soc/|pm_wakeirq|leds|page_pool)'
KPI_SOURCE_GROUPS = (
    ('net', r'80211|netdev|skbuff|linux_usb|linux_mhi|page_pool'),
    ('firmware', r'linux_firmware'),
    ('pci', r'linux_pci|linux_aperture'),
    ('irq', r'linux_interrupt|linux_tasklet'),
    ('time', r'linux_hrtimer|linux_timesub'),
    ('work', r'linux_work|linux_kthread|linux_schedule|linux_current'),
    ('lock', r'linux_lock|linux_rcu'),
    ('alloc', r'linux_slab'),
    ('mm', r'linux_page|linux_shmemfs|linux_shrinker|linux_folio'),
    ('file', r'linux_simple_attr|linux_seq_file|linux_seq_buf|linux_eventfd'),
    ('data', r'linux_idr|linux_xarray|linux_radix'),
    ('acpi', r'linux_acpi|linux_dmi'),
    ('arch', r'linux_fpu'),
    ('console', r'linux_cmdline|videokmod'),
    ('bus', r'linux_i2c|linux_hdmi|hdmikmod|lkpi_iic'),
    ('util', r'linux_math|linux_siphash'),
    ('devmodel', r'.'),
)


def clean(text):
    text = text.replace('\\\n', ' ')
    return STRIP.sub(lambda m: ' ' if m.group(0)[0] == '/' else '0', text)


def read(path):
    size = path.stat().st_size
    assert size <= MAX_FILE_BYTES, f'{path} is larger than {MAX_FILE_BYTES} bytes'
    return path.read_text(encoding='utf-8', errors='replace')


def walk(root, suffixes):
    files = sorted(p for p in root.rglob('*') if p.is_file() and p.suffix in suffixes)
    assert len(files) <= MAX_FILES, f'{root} holds more than {MAX_FILES} files'
    return files


def depth_zero_names(body):
    names = set()
    depth = 0
    for match in SCAN.finditer(body):
        token = match.group(0)
        start = match.start()
        at_line_start = start == 0 or body[start - 1] == '\n'
        if token == '{':
            depth = 1 if at_line_start else depth + 1
        elif token == '}':
            depth = 0 if at_line_start else max(depth - 1, 0)
        elif depth == 0 and match.group(1) not in KEYWORDS:
            names.add(match.group(1))
    return names


class Definitions:
    def __init__(self):
        self.function_macros = defaultdict(list)
        self.object_macros = defaultdict(list)
        self.functions = defaultdict(list)
        self.structs = defaultdict(list)
        self.enumerators = defaultdict(list)

    def add(self, text, origin):
        for match in DEFINE.finditer(text):
            target = self.function_macros if match.group(2) else self.object_macros
            target[match.group(1)].append(origin)
        body = PREPROC.sub('', text)
        for name in depth_zero_names(body):
            self.functions[name].append(origin)
        for match in STRUCT_DEF.finditer(body):
            self.structs[match.group(1)].append(origin)
        for match in ENUM.finditer(body):
            for item in ENUMERATOR.finditer(match.group(1)):
                self.enumerators[item.group(1)].append(origin)

    def callable_origins(self, name):
        macros = self.function_macros.get(name, []) + self.object_macros.get(name, [])
        return macros if macros else self.functions.get(name, [])

    def constant_origins(self, name):
        return self.object_macros.get(name, []) + self.enumerators.get(name, [])

    def names(self):
        return set(self.function_macros) | set(self.object_macros) | set(self.functions)

    def constants(self):
        return set(self.object_macros) | set(self.enumerators) | set(self.function_macros)

    def types(self):
        return set(self.structs) | set(self.object_macros) | set(self.function_macros)


def header_priority(rel):
    order = ('include/linux/', 'include/asm-generic/', 'arch/x86/include/asm/', 'include/acpi/', 'include/video/',
             'include/sound/', 'include/uapi/', 'arch/x86/include/uapi/', 'include/')
    for rank, prefix in enumerate(order):
        if rel.startswith(prefix):
            return rank
    return len(order)


def include_name(rel):
    for prefix in ('arch/x86/include/uapi/', 'arch/x86/include/', 'include/'):
        if rel.startswith(prefix):
            return rel[len(prefix):]
    return rel


def group_of(name):
    for key, _label, pattern in GROUPS:
        if re.search(pattern, name):
            return key
    return 'other'


def best_origin(origins, included):
    ranked = sorted(enumerate(origins), key=lambda item: (item[1] not in included, group_of(item[1]) == 'other', item[0]))
    return ranked[0][1]


def kpi_group_of(rel):
    if '/include/' in rel:
        name = rel.split('/include/', 1)[1]
        return 'net' if re.search(NOT_FOR_A_GPU, name) else group_of(name)
    for key, pattern in KPI_SOURCE_GROUPS:
        if re.search(pattern, rel):
            return key
    return 'devmodel'


def drm_kmod_sources(drm_kmod):
    built = {}
    for module, makefile in (('dmabuf', 'dmabuf/Makefile'), ('ttm', 'ttm/Makefile'), ('drm', 'drm/Makefile'),
                             ('amdgpu', 'amd/amdgpu/Makefile')):
        path = drm_kmod / makefile
        assert path.is_file(), f'{path} is missing: run fetch.sh'
        names = set()
        text = read(path).replace('\\\n', ' ')
        for line in text.splitlines():
            if re.match(r'^\s*SRCS\s*\+?=', line):
                names.update(SRCS.findall(line))
        built[module] = names
    return built


def openbsd_sources(openbsd):
    path = openbsd / 'sys/dev/pci/drm/files.drm'
    assert path.is_file(), f'{path} is missing: run fetch.sh'
    names = {name.rsplit('/', 1)[-1] for name in re.findall(r'^file\s+dev/pci/drm/(\S+\.c)', read(path), re.M)}
    return {module: names for module in ('dmabuf', 'ttm', 'drm', 'amdgpu')}


def openbsd_options(openbsd):
    path = openbsd / 'sys/dev/pci/drm/include/generated/autoconf.h'
    assert path.is_file(), f'{path} is missing: run fetch.sh'
    return set(re.findall(r'^#define\s+CONFIG_(\w+)', read(path), re.M))


def scope_of(rel):
    if rel.startswith('drivers/gpu/drm/amd/'):
        return None if rel.startswith(AMD_EXCLUDED) else 'amdgpu'
    if rel.startswith('drivers/gpu/drm/virtio/'):
        return 'virtio'
    if rel.startswith('drivers/gpu/drm/nouveau/'):
        return 'nouveau'
    if rel.startswith('drivers/gpu/drm/ttm/'):
        return None if '/tests/' in rel else 'ttm'
    if rel.startswith('drivers/gpu/drm/scheduler/'):
        return None if '/tests/' in rel else 'sched'
    if rel.startswith(('drivers/gpu/drm/display/', 'drivers/gpu/drm/clients/')):
        return 'drm'
    if rel.startswith('drivers/gpu/drm/') and rel.count('/') == 3:
        return 'drm'
    if rel.startswith('drivers/dma-buf/') and rel.count('/') == 2:
        return 'dmabuf' if rel.rsplit('/', 1)[1] in DMABUF_FILES else None
    if rel.startswith(SET_HEADERS):
        return 'drm'
    return None


KBUILD_CONDITIONAL = re.compile(r'^[\w-]+-\$\(CONFIG_(\w+)\)\s*[:+]?=\s*(.+)$', re.M)
KBUILD_COMPOSITE = re.compile(r'^([\w-]+)-y\s*[:+]?=\s*(.+)$', re.M)


def configured_out(set_root, options):
    excluded = set()
    for scope in sorted(UNFILTERED_SCOPES):
        base = set_root / 'drivers/gpu/drm' / scope
        if not base.is_dir():
            continue
        for kbuild in sorted(p for p in base.rglob('*') if p.name in ('Kbuild', 'Makefile')):
            text = read(kbuild).replace('\\\n', ' ')
            composites = {name: parts.split() for name, parts in KBUILD_COMPOSITE.findall(text)}
            for option, objects in KBUILD_CONDITIONAL.findall(text):
                if option in options:
                    continue
                for item in objects.split():
                    stem = item.rsplit('/', 1)[-1].removesuffix('.o')
                    for part in [item] + composites.get(stem, []):
                        excluded.add(part.rsplit('/', 1)[-1].removesuffix('.o') + '.c')
    return excluded


def collect_set(set_root, layout, stack, built, extra, excluded=frozenset()):
    chosen = []
    skipped = []
    for logical, physical in LAYOUTS[layout]:
        base = set_root / physical
        if not base.is_dir():
            continue
        for path in walk(base, {'.c', '.h'}):
            rel = logical + '/' + path.relative_to(base).as_posix()
            scope = scope_of(rel)
            if scope is None or scope not in stack:
                continue
            name = rel.rsplit('/', 1)[1]
            if path.suffix == '.c' and built is not None:
                if scope in UNFILTERED_SCOPES:
                    listed = name not in excluded
                else:
                    listed = name in extra or any(name in names for names in built.values())
                if not listed:
                    skipped.append(rel)
                    continue
            chosen.append((scope, rel, path))
    return chosen, skipped


def license_of(text):
    head = text[:6000]
    match = SPDX.search(head)
    if match:
        return match.group(1).strip().rstrip('*/ ').strip()
    if 'Permission is hereby granted, free of charge' in head:
        return 'MIT (permission notice, no SPDX line)'
    if 'GNU General Public License' in head:
        return 'GPL (notice, no SPDX line)'
    if 'Redistribution and use in source and binary forms' in head:
        return 'BSD (notice, no SPDX line)'
    if 'Permission to use, copy, modify, distribute, and sell this software' in head:
        return 'HPND (permission notice, no SPDX line)'
    if 'Permission to use, copy, modify, and distribute this software for any' in head:
        return 'ISC (notice, no SPDX line)'
    if 'Public domain' in head[:200]:
        return 'public domain'
    return 'none stated'


def scan_body(text):
    def keep(match):
        line = match.group(0)
        directive = line.lstrip('# \t')
        return line if directive.startswith(('define', 'if', 'elif')) else ''
    return PREPROC.sub(keep, text)


CONDITION = re.compile(r'^[ \t]*#[ \t]*(ifdef|ifndef|if|elif|else|endif)\b(.*)$')
CONDITION_TOKEN = re.compile(r'&&|\|\||!(?!=)|\(|\)|[A-Za-z_]\w*|\S')
ENABLED = frozenset(('IS_ENABLED', 'IS_REACHABLE', 'IS_BUILTIN', 'defined'))
MAX_CONDITION_DEPTH = 64


def kconfig_of(drm_kmod):
    path = drm_kmod / 'kconfig.mk'
    assert path.is_file(), f'{path} is missing: run fetch.sh'
    names = set()
    live = [True]
    for line in read(path).replace('\\\n', ' ').splitlines():
        stripped = line.strip()
        if stripped.startswith('.if'):
            live.append(live[-1] and ('amd64' in stripped or 'NO_FBDEV' in stripped))
            assert len(live) <= MAX_CONDITION_DEPTH, f'{path} nests conditionals too deep'
        elif stripped.startswith('.endif'):
            live.pop()
        elif live[-1] and re.match(r'^KCONFIG\s*\+?=', stripped):
            for item in stripped.split('=', 1)[1].split():
                names.add(item.split('=', 1)[0])
    return names


def three_and(left, right):
    if left is False or right is False:
        return False
    return True if left is True and right is True else None


def three_or(left, right):
    if left is True or right is True:
        return True
    return False if left is False and right is False else None


class Condition:
    def __init__(self, text, known):
        self.tokens = CONDITION_TOKEN.findall(text)
        self.at = 0
        self.known = known
        self.failed = False

    def take(self, expected=None):
        if self.at >= len(self.tokens) or (expected is not None and self.tokens[self.at] != expected):
            self.failed = True
            return None
        self.at += 1
        return self.tokens[self.at - 1]

    def peek(self):
        return self.tokens[self.at] if self.at < len(self.tokens) else None

    def value(self):
        result = self.either()
        return None if self.failed or self.at != len(self.tokens) else result

    def either(self):
        result = self.both()
        while self.peek() == '||':
            self.take()
            result = three_or(result, self.both())
        return result

    def both(self):
        result = self.unary()
        while self.peek() == '&&':
            self.take()
            result = three_and(result, self.unary())
        return result

    def unary(self):
        token = self.take()
        if token == '!':
            inner = self.unary()
            return None if inner is None else not inner
        if token == '(':
            inner = self.either()
            self.take(')')
            return inner
        if token in ENABLED:
            bracket = self.peek() == '('
            if bracket:
                self.take()
            name = self.take()
            if bracket:
                self.take(')')
            return None if name is None else self.known(name)
        if token in ('0', '1'):
            return token == '1'
        self.failed = True
        return None


def compiled(text, known):
    out = []
    stack = []
    live = True
    for line in text.split('\n'):
        match = CONDITION.match(line)
        if match is None:
            out.append(line if live else '')
            continue
        directive, rest = match.group(1), match.group(2)
        if directive in ('if', 'ifdef', 'ifndef'):
            if directive == 'if':
                verdict = Condition(rest, known).value()
            else:
                verdict = known(rest.strip().split()[0]) if rest.strip() else None
                if directive == 'ifndef' and verdict is not None:
                    verdict = not verdict
            stack.append([live, verdict is True])
            assert len(stack) <= MAX_CONDITION_DEPTH, 'conditionals nest too deep'
            live = live and verdict is not False
        elif directive in ('elif', 'else') and stack:
            parent, taken = stack[-1]
            verdict = True if directive == 'else' else Condition(rest, known).value()
            live = parent and not taken and verdict is not False
            if verdict is True:
                stack[-1][1] = True
        elif directive == 'endif' and stack:
            live = stack.pop()[0]
        out.append(line if live else '')
    return '\n'.join(out)


def is_stub(text):
    return not PREPROC.sub('', text).strip() and len(DEFINE.findall(text)) <= 1


def analyse(args):
    linux = Path(args.linux)
    set_root = Path(args.set_root) if args.set_root else linux
    work = Path(args.work)
    freebsd = work / 'freebsd-src' / 'sys'
    openbsd = work / 'openbsd-src'
    drm_kmod = Path(args.drm_kmod) if args.drm_kmod else work / 'drm-kmod'
    stack = STACKS[args.stack]
    extra = STACK_EXTRA.get(args.stack, frozenset())
    if args.layer == 'openbsd':
        built = None if args.whole_directories else openbsd_sources(openbsd)
        kconfig = None if args.every_configuration else openbsd_options(openbsd)
        port_macro = '__OpenBSD__'
    else:
        built = None if args.whole_directories else drm_kmod_sources(drm_kmod)
        kconfig = None if args.every_configuration else kconfig_of(drm_kmod)
        port_macro = '__FreeBSD__'
    if built is not None and args.files_of_both_ports:
        other = drm_kmod_sources(drm_kmod) if args.layer == 'openbsd' else openbsd_sources(openbsd)
        built = {module: built[module] | other[module] for module in built}
    if kconfig is not None:
        kconfig |= STACK_OPTIONS.get(args.stack, frozenset())
    forced = {port_macro: True, '__linux__': False} if args.as_ported else {}
    if forced:
        forced.update({name: False for name in NEVER_DEFINED})

    def known(name):
        if name in forced:
            return forced[name]
        if kconfig is not None and name.startswith('CONFIG_'):
            return name[7:] in kconfig
        return None

    excluded = configured_out(set_root, kconfig) if kconfig is not None and args.layout == 'linux' else frozenset()
    chosen, skipped = collect_set(set_root, args.layout, stack, built, extra, excluded)
    assert chosen, f'no set files under {set_root}'

    set_defs = Definitions()
    everything, _ = collect_set(set_root, args.layout, SCOPES, None, extra)
    cleaned = {}
    set_paths = []
    for _scope, rel, path in everything:
        text = clean(read(path))
        cleaned[rel] = text
        set_paths.append('/' + rel)
        set_defs.add(text, rel)
    asic_reg = set_root / 'drivers/gpu/drm/amd/include/asic_reg'
    if asic_reg.is_dir():
        set_paths.extend('/' + p.relative_to(set_root).as_posix() for p in walk(asic_reg, {'.h'}))
    set_suffixes = set()
    for path in set_paths:
        parts = path.split('/')
        for start in range(1, len(parts)):
            set_suffixes.add('/'.join(parts[start:]))

    calls = Counter()
    call_files = defaultdict(set)
    refs = defaultdict(set)
    declared = set()
    idents = defaultdict(set)
    struct_uses = defaultdict(set)
    includes = Counter()
    include_files = defaultdict(set)
    lines = Counter()
    files = Counter()
    licenses = defaultdict(Counter)
    port_conditionals = 0
    port_files = 0
    for scope, rel, path in chosen:
        raw = read(path)
        text = cleaned[rel] if kconfig is None and not forced else compiled(cleaned[rel], known)
        lines[scope] += raw.count('\n')
        files[scope + path.suffix] += 1
        licenses[scope][license_of(raw)] += 1
        found = len(PORT_CONDITIONAL.findall(cleaned[rel]))
        port_conditionals += found
        port_files += 1 if found else 0
        for match in INCLUDE.finditer(text):
            includes[match.group(1)] += 1
            include_files[match.group(1)].add(rel)
        body = scan_body(text)
        for match in CALL.finditer(body):
            name = match.group(1)
            if name not in KEYWORDS:
                calls[name] += 1
                call_files[name].add(rel)
        for match in REF.finditer(body):
            refs[match.group(1)].add(rel)
        for match in DECLARED.finditer(body):
            if match.group(1) not in NOT_A_TYPE:
                declared.add(match.group(2))
        for match in IDENT.finditer(body):
            idents[match.group(1)].add(rel)
        for match in STRUCT_USE.finditer(body):
            struct_uses[match.group(1)].add(rel)

    external = Definitions()
    headers = []
    for sub in ('include', 'arch/x86/include'):
        for path in walk(linux / sub, {'.h'}):
            rel = path.relative_to(linux).as_posix()
            if not rel.startswith(SET_HEADERS):
                headers.append((header_priority(rel), rel, path))
    headers.sort()
    known_headers = set()
    for _rank, rel, path in headers:
        known_headers.add(include_name(rel))
        external.add(clean(read(path)), include_name(rel))

    kpi = Definitions()
    kpi_lines = Counter()
    kpi_files = Counter()
    kpi_licenses = Counter()
    kpi_headers = {}
    dummy_headers = set()
    if args.layer == 'openbsd':
        drm = openbsd / 'sys/dev/pci/drm'
        layer_files = [(path, path.relative_to(drm / 'include').as_posix()) for path in walk(drm / 'include', {'.h'})
                       if not path.relative_to(drm).as_posix().startswith(('include/drm/', 'include/uapi/drm/'))]
        layer_files += [(path, None) for path in sorted(drm.glob('*.c')) if path.name.startswith(('drm_linux', 'linux_'))]
        native_roots = [openbsd / 'sys' / sub for sub in ('sys', 'lib/libkern', 'arch/amd64/include', 'uvm', 'dev/pci')]
        native_base = openbsd / 'sys'
        for path, header in layer_files:
            raw = read(path)
            text = clean(raw)
            kpi.add(text, path.relative_to(drm).as_posix())
            kpi_lines[group_of(header) if header else 'devmodel'] += raw.count('\n')
            kpi_files['openbsd' + path.suffix] += 1
            kpi_licenses[license_of(raw)] += 1
            if header and is_stub(text):
                dummy_headers.add(header)
            elif header:
                kpi_headers[header] = 'openbsd'
    else:
        for owner, root in (('freebsd', freebsd / 'compat/linuxkpi/common'), ('drm-kmod', drm_kmod / 'linuxkpi')):
            assert root.is_dir(), f'{root} is missing: run fetch.sh'
            for path in walk(root, {'.c', '.h'}):
                rel = path.relative_to(root.parent).as_posix()
                raw = read(path)
                kpi.add(clean(raw), rel)
                kpi_lines[kpi_group_of(rel)] += raw.count('\n')
                kpi_files[owner + path.suffix] += 1
                kpi_licenses[license_of(raw)] += 1
                if '/include/' in rel:
                    kpi_headers[rel.split('/include/', 1)[1]] = owner
        dummy_root = freebsd / 'compat/linuxkpi/dummy/include'
        dummy_headers = {p.relative_to(dummy_root).as_posix() for p in walk(dummy_root, {'.h'})}
        native_roots = [freebsd / sub for sub in ('sys', 'libkern', 'x86/include', 'amd64/include', 'vm', 'dev/pci')]
        native_base = freebsd

    native = Definitions()
    for root in native_roots:
        assert root.is_dir(), f'{root} is missing: run fetch.sh'
        for path in walk(root, {'.h'}):
            native.add(clean(read(path)), path.relative_to(native_base).as_posix())

    exported = {}
    exports_file = work / f'exports-{args.version}.txt'
    assert exports_file.is_file(), f'{exports_file} is missing: run fetch.sh'
    for line in read(exports_file).splitlines():
        name, _, origin = line.partition(' ')
        exported.setdefault(name, origin)

    set_names = set_defs.names()
    kpi_names = kpi.names()
    native_names = native.names()
    included = set(includes)

    def status(name, kpi_set, native_set):
        if name in kpi_set:
            return 'layer'
        if name in native_set:
            return 'native'
        return 'missing'

    interfaces = {}
    for name, count in calls.items():
        if name in set_names:
            continue
        origins = external.callable_origins(name)
        if not origins:
            continue
        interfaces[name] = {'kind': 'call', 'origin': best_origin(origins, included), 'sites': count,
                            'files': len(call_files[name])}
    function_only = set(external.functions) - set(external.function_macros) - set(external.object_macros)
    for name, where in refs.items():
        if name in interfaces or name in set_names or name not in function_only or name in declared:
            continue
        if name not in exported or exported[name].startswith(('drivers/gpu/drm/', 'drivers/dma-buf/')):
            continue
        interfaces[name] = {'kind': 'reference', 'origin': best_origin(external.functions[name], included),
                            'sites': len(where), 'files': len(where)}
    for name, info in interfaces.items():
        info['group'] = group_of(info['origin'])
        info['status'] = status(name, kpi_names, native_names)
        info['exported'] = name in exported

    set_constants = set_defs.constants()
    kpi_constants = kpi.constants() | kpi_names
    native_constants = native.constants() | native_names
    constants = {}
    for name, where in idents.items():
        if name in interfaces or name in set_constants or name in set_names or name in KEYWORDS:
            continue
        origins = external.constant_origins(name)
        if not origins:
            continue
        origin = best_origin(origins, included)
        constants[name] = {'origin': origin, 'group': group_of(origin), 'files': len(where),
                           'status': status(name, kpi_constants, native_constants)}

    types = {}
    for name, where in struct_uses.items():
        if name in set_defs.structs or name not in external.structs:
            continue
        origin = best_origin(external.structs[name], included)
        types[name] = {'origin': origin, 'group': group_of(origin), 'files': len(where),
                       'status': status(name, kpi.types(), native.types())}

    header_rows = {}
    own_headers = 0
    foreign_headers = []
    for name, count in includes.items():
        if name in set_suffixes:
            own_headers += 1
            continue
        if name not in known_headers and not name.startswith('generated/'):
            foreign_headers.append(name)
            continue
        if name in kpi_headers:
            state = 'layer'
        elif name in dummy_headers:
            state = 'empty'
        else:
            state = 'missing'
        header_rows[name] = {'group': group_of(name), 'includes': count, 'files': len(include_files[name]),
                             'status': state}

    symbols = {name: info for name, info in interfaces.items()
               if info['exported'] and not exported[name].startswith(('drivers/gpu/drm/', 'drivers/dma-buf/'))}

    def tally(rows):
        counter = Counter(info['status'] for info in rows.values())
        counter['needed'] = len(rows)
        return dict(counter)

    def by_group(rows, sites_key):
        table = defaultdict(Counter)
        for info in rows.values():
            table[info['group']][info['status']] += 1
            table[info['group']]['needed'] += 1
            table[info['group']]['sites'] += info[sites_key]
        return {key: dict(value) for key, value in table.items()}

    group_files = defaultdict(set)
    group_top = defaultdict(list)
    for name, info in interfaces.items():
        group_files[info['group']].update(call_files.get(name, ()))
        group_top[info['group']].append((info['sites'], name))
    usage = {}
    for key, entries in group_top.items():
        entries.sort(reverse=True)
        usage[key] = {'files': len(group_files[key]), 'top': [name for _sites, name in entries[:TOP_NAMES]]}

    result = {
        'linux': args.version,
        'stack': args.stack,
        'layer': args.layer,
        'set_root': str(set_root),
        'filter': ('whole directories' if built is None else 'the C files either port builds' if args.files_of_both_ports
                   else f'the C files the {args.layer} port builds') + ', '
                  + ('every configuration' if kconfig is None else f'the {len(kconfig)} options of the {args.layer} port on amd64')
                  + (f', as ported ({port_macro} defined, __linux__ not)' if forced else ''),
        'set': {'files': dict(files), 'lines': dict(lines), 'c_files_left_out': len(skipped),
                'licenses': {scope: dict(counter) for scope, counter in licenses.items()},
                'port_conditionals': port_conditionals, 'files_with_port_conditionals': port_files},
        'linux_headers_read': len(headers),
        'layer_size': {'files': dict(kpi_files), 'lines_by_group': dict(kpi_lines), 'lines': sum(kpi_lines.values()),
                       'licenses': dict(kpi_licenses), 'empty_headers': len(dummy_headers), 'names': len(kpi_names)},
        'totals': {'headers': tally(header_rows), 'interfaces': tally(interfaces), 'constants': tally(constants),
                   'types': tally(types), 'exported_symbols': tally(symbols)},
        'groups': {'headers': by_group(header_rows, 'includes'), 'interfaces': by_group(interfaces, 'sites'),
                   'constants': by_group(constants, 'files'), 'types': by_group(types, 'files'), 'usage': usage},
        'own_headers_in_angle_brackets': own_headers,
        'headers_outside_the_kernel': sorted(foreign_headers),
        'left_out': sorted(skipped)[:MAX_LISTED],
        'headers': header_rows,
        'interfaces': interfaces,
        'constants': constants,
        'types': types,
    }
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(result, indent=1, sort_keys=True) + '\n')
    report(result)
    return 0


def report(result):
    totals = result['totals']
    setinfo = result['set']
    print(f"Linux {result['linux']}, stack {result['stack']}, {result['filter']}, set files from {result['set_root']}")
    print(f"set: {sum(setinfo['files'].values())} files, {sum(setinfo['lines'].values())} lines, "
          f"{setinfo['c_files_left_out']} C files left out, {setinfo['port_conditionals']} port conditionals "
          f"in {setinfo['files_with_port_conditionals']} files")
    print()
    print(f"|What|Needed|In the {result['layer']} layer|Empty header|Native on that kernel|Missing|")
    print('|---|---|---|---|---|---|')
    labels = (('headers', 'Headers included'), ('interfaces', 'Functions and function-like macros'),
              ('exported_symbols', 'of those, exported kernel symbols'), ('constants', 'Constants and object-like macros'),
              ('types', 'Structure types'))
    for key, label in labels:
        row = totals[key]
        print(f"|{label}|{row.get('needed', 0)}|{row.get('layer', 0)}|{row.get('empty', 0)}|{row.get('native', 0)}|{row.get('missing', 0)}|")
    print()
    print('|Group|Headers|missing|Functions|in the layer|native|missing|call sites|files|layer lines|')
    print('|---|---|---|---|---|---|---|---|---|---|')
    names = {key: label for key, label, _pattern in GROUPS}
    names['other'] = 'Other'
    for key in [key for key, _label, _pattern in GROUPS] + ['other']:
        headers = result['groups']['headers'].get(key, {})
        functions = result['groups']['interfaces'].get(key, {})
        if not headers and not functions:
            continue
        usage = result['groups']['usage'].get(key, {})
        print(f"|{names[key]}|{headers.get('needed', 0)}|{headers.get('missing', 0)}|{functions.get('needed', 0)}|"
              f"{functions.get('layer', 0)}|{functions.get('native', 0)}|{functions.get('missing', 0)}|"
              f"{functions.get('sites', 0)}|{usage.get('files', 0)}|{result['layer_size']['lines_by_group'].get(key, 0)}|")


def exports(args):
    found = {}
    members = 0
    with tarfile.open(args.tarball, 'r|xz') as archive:
        for member in archive:
            members += 1
            assert members <= MAX_TAR_MEMBERS, f'{args.tarball} holds more than {MAX_TAR_MEMBERS} members'
            if not member.isfile() or not member.name.endswith(('.c', '.h', '.S')):
                continue
            if member.size > MAX_FILE_BYTES:
                continue
            data = archive.extractfile(member).read()
            if b'EXPORT_SYMBOL' not in data:
                continue
            rel = member.name.split('/', 1)[1]
            for match in EXPORT.finditer(data.decode('utf-8', errors='replace')):
                found.setdefault(match.group(1), rel)
    Path(args.out).write_text(''.join(f'{name} {origin}\n' for name, origin in sorted(found.items())))
    print(f'{len(found)} exported symbols from {members} members of {args.tarball}')
    return 0


def compare(args):
    base = json.loads(Path(args.base).read_text())
    other = json.loads(Path(args.other).read_text())
    print(f"|What|At {base['linux']}|At {other['linux']}|New|New and missing|No longer used|Missing at both|")
    print('|---|---|---|---|---|---|---|')
    for key in ('headers', 'interfaces', 'constants', 'types'):
        gone = set(base[key]) - set(other[key])
        new = set(other[key]) - set(base[key])
        new_missing = sorted(name for name in new if other[key][name]['status'] == 'missing')
        still = [name for name in set(base[key]) & set(other[key])
                 if other[key][name]['status'] == 'missing' and base[key][name]['status'] == 'missing']
        print(f"|{key}|{len(base[key])}|{len(other[key])}|{len(new)}|{len(new_missing)}|{len(gone)}|{len(still)}|")
        if args.names:
            print('new and missing: ' + ' '.join(new_missing[:MAX_LISTED]))
    return 0


def extra(args):
    base = json.loads(Path(args.base).read_text())
    other = json.loads(Path(args.other).read_text())
    names = {key: label for key, label, _pattern in GROUPS}
    names['other'] = 'Other'
    print(f"what the {other['stack']} stack needs beyond the {base['stack']} stack, Linux {other['linux']}, {other['layer']} layer")
    print('|What|Extra|In the layer|Native|Missing|')
    print('|---|---|---|---|---|')
    for key in ('headers', 'interfaces', 'constants', 'types'):
        rows = [other[key][name] for name in set(other[key]) - set(base[key])]
        tally = Counter(row['status'] for row in rows)
        print(f"|{key}|{len(rows)}|{tally['layer'] + tally['empty']}|{tally['native']}|{tally['missing']}|")
    print()
    print('|Group|Extra functions|In the layer|Native|Missing|Missing names|')
    print('|---|---|---|---|---|---|')
    table = defaultdict(list)
    for name in set(other['interfaces']) - set(base['interfaces']):
        table[other['interfaces'][name]['group']].append(name)
    for key in [key for key, _label, _pattern in GROUPS] + ['other']:
        if key not in table:
            continue
        rows = table[key]
        tally = Counter(other['interfaces'][name]['status'] for name in rows)
        missing = sorted(name for name in rows if other['interfaces'][name]['status'] == 'missing')
        print(f"|{names[key]}|{len(rows)}|{tally['layer']}|{tally['native']}|{tally['missing']}|{' '.join(missing)}|")
    return 0


def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest='command', required=True)
    count = commands.add_parser('count')
    count.add_argument('--work', required=True)
    count.add_argument('--version', required=True)
    count.add_argument('--linux', required=True)
    count.add_argument('--set-root')
    count.add_argument('--drm-kmod')
    count.add_argument('--layer', choices=('freebsd', 'openbsd'), default='freebsd')
    count.add_argument('--layout', choices=sorted(LAYOUTS), default='linux')
    count.add_argument('--stack', choices=sorted(STACKS), default='amdgpu')
    count.add_argument('--whole-directories', action='store_true')
    count.add_argument('--every-configuration', action='store_true')
    count.add_argument('--as-ported', action='store_true')
    count.add_argument('--files-of-both-ports', action='store_true')
    count.add_argument('--out', required=True)
    count.set_defaults(run=analyse)
    export = commands.add_parser('exports')
    export.add_argument('tarball')
    export.add_argument('out')
    export.set_defaults(run=exports)
    diff = commands.add_parser('compare')
    diff.add_argument('base')
    diff.add_argument('other')
    diff.add_argument('--names', action='store_true')
    diff.set_defaults(run=compare)
    more = commands.add_parser('extra')
    more.add_argument('base')
    more.add_argument('other')
    more.set_defaults(run=extra)
    args = parser.parse_args()
    return args.run(args)


if __name__ == '__main__':
    sys.exit(main())
