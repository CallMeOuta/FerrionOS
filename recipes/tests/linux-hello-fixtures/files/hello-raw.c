static long sc3(long n, long a, long b, long c) {
    long r; __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}
void _start(void) {
    static const char m[] = "hello from linux\n";
    sc3(1, 1, (long)m, sizeof m - 1);
    sc3(231, 0, 0, 0);
    for (;;) {}
}
