// The per-thread crossing between Linux code and the runtime (plans/linux-compat.md, "How a
// system call travels"). This template is copied into a fresh code page for every Linux thread;
// the page after it holds that thread's data, which the copy reaches RIP-relative, because
// neither FS (owned by the Linux C library) nor GS (a Linux program may set it) can be used.
//
// Data page, at lr_stub_start + 4096:
//   +0   Linux RSP at entry; RSP to resume with at exit
//   +8   top of the thread's native stack
//   +16  Linux FS base
//   +24  native FS base (relibc's thread block)
//   +32  the Rust dispatcher, lr_dispatch(frame, data)
//   +40  RIP to resume Linux code at
//   +48  the thread's runtime state
//   +4096 XSAVE area
//
// The kernel enters lr_stub_start when this thread executes `syscall` outside the native window,
// with every register as the instruction left it: RCX the return address, R11 the flags. The
// frame pushed below is `Frame` in src/thread.rs.
    .section .text.lr_stub, "ax"
    .balign 4096
    .globl lr_stub_start
lr_stub_start:
    mov [rip + lr_stub_start + 4096 + 0], rsp
    mov rsp, [rip + lr_stub_start + 4096 + 8]
    push qword ptr [rip + lr_stub_start + 4096 + 0]
    push r11
    push rcx
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    rdfsbase rax
    mov [rip + lr_stub_start + 4096 + 16], rax
    mov rax, [rip + lr_stub_start + 4096 + 24]
    wrfsbase rax
    mov eax, -1
    mov edx, -1
    xsave64 [rip + lr_stub_start + 8192]
    cld
    mov rdi, rsp
    lea rsi, [rip + lr_stub_start + 4096]
    call qword ptr [rip + lr_stub_start + 4096 + 32]

    // Also the way into Linux code for a new thread: RSP points at a complete frame.
    .globl lr_stub_exit
lr_stub_exit:
    mov eax, -1
    mov edx, -1
    xrstor64 [rip + lr_stub_start + 8192]
    mov rax, [rip + lr_stub_start + 4096 + 16]
    wrfsbase rax
    mov rax, [rsp + 15 * 8]
    mov [rip + lr_stub_start + 4096 + 40], rax
    mov rax, [rsp + 17 * 8]
    mov [rip + lr_stub_start + 4096 + 0], rax
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 8
    popfq
    mov rsp, [rip + lr_stub_start + 4096 + 0]
    jmp qword ptr [rip + lr_stub_start + 4096 + 40]
    .globl lr_stub_end
lr_stub_end:
    .balign 4096
    // Two pages that the template's RIP-relative operands name; never used themselves.
    .skip 8192
    .text
