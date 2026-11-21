// The per-thread crossing between Linux code and the runtime (plans/linux-compat.md, "How a
// system call travels" and "Signals"). This template is copied into a fresh code page for every
// Linux thread; the page after it holds that thread's data, which the copy reaches RIP-relative,
// because neither FS (owned by the Linux C library) nor GS (a Linux program may set it) can be
// used.
//
// Data page, at lr_stub_start + 4096 (src/thread.rs names the same offsets):
//   +0   Linux RSP at entry; RSP to resume with at exit
//   +8   top of the thread's native stack
//   +16  Linux FS base
//   +24  native FS base (relibc's thread block)
//   +32  the Rust dispatcher, lr_dispatch(frame, data)
//   +40  RIP to resume Linux code at
//   +48  the thread's runtime state
//   +56  relibc's signal control block of this thread (Sigcontrol)
//   +64  relibc's signal entry
//   +72  relibc's exception entry
//   +80  lr_interrupt(frame, data): a native signal arrived while Linux code ran
//   +88  lr_fault(frame, data): Linux code faulted
//   +96  E_RAX, +104 E_RSP, +112 E_RIP, +120 E_FLAGS: the interrupted state, saved at entry
//   +128 S_RAX, +136 S_RSP, +144 S_RIP, +152 S_FLAGS: the state a deferred signal resumes
//   +4096 XSAVE area
//
// The kernel enters lr_stub_start when this thread executes `syscall` outside the native window,
// with every register as the instruction left it: RCX the return address, R11 the flags. The
// frame pushed below is `Frame` in src/thread.rs.
//
// The runtime registers lr_sig_entry and lr_excp_entry as this thread's signal and exception
// entries (instead of relibc's, which write through FS). The kernel enters them with every
// register as interrupted except RIP, the interrupted RIP and RFLAGS in Sigcontrol's saved_ip
// and saved_archdep_reg, and delivery inhibited until the entry clears it. Sigcontrol offsets
// (redox_syscall's sigabi.rs): control_flags 272, saved_ip 280, saved_archdep_reg 288,
// saved_excp_code 296, saved_excp_addr 304.
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

// Saves RAX, RSP and the interrupted RIP and RFLAGS in the E slots; uses RAX and RSP as scratch.
.macro LR_SAVE_INTERRUPTED
    mov [rip + lr_stub_start + 4096 + 96], rax
    mov [rip + lr_stub_start + 4096 + 104], rsp
    mov rsp, [rip + lr_stub_start + 4096 + 56]
    mov rax, [rsp + 280]
    mov [rip + lr_stub_start + 4096 + 112], rax
    mov rax, [rsp + 288]
    mov [rip + lr_stub_start + 4096 + 120], rax
.endm

// Builds a Frame from the interrupted state on the native stack, swaps FS and saves the vector
// state, as the system-call entry does. RCX and R11 keep their interrupted values.
.macro LR_ENTER_FROM_LINUX
    mov rsp, [rip + lr_stub_start + 4096 + 8]
    push qword ptr [rip + lr_stub_start + 4096 + 104]
    push qword ptr [rip + lr_stub_start + 4096 + 120]
    push qword ptr [rip + lr_stub_start + 4096 + 112]
    push qword ptr [rip + lr_stub_start + 4096 + 96]
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
.endm

    .globl lr_sig_entry
lr_sig_entry:
    LR_SAVE_INTERRUPTED
    // Interrupted while resuming a deferred signal: the state to resume is the S slots.
    mov rax, [rip + lr_stub_start + 4096 + 112]
    lea rsp, [rip + lr_defer_crit]
    cmp rax, rsp
    jb 1f
    lea rsp, [rip + lr_defer_end]
    cmp rax, rsp
    jae 1f
    mov rax, [rip + lr_stub_start + 4096 + 128]
    mov [rip + lr_stub_start + 4096 + 96], rax
    mov rax, [rip + lr_stub_start + 4096 + 136]
    mov [rip + lr_stub_start + 4096 + 104], rax
    mov rax, [rip + lr_stub_start + 4096 + 144]
    mov [rip + lr_stub_start + 4096 + 112], rax
    mov rax, [rip + lr_stub_start + 4096 + 152]
    mov [rip + lr_stub_start + 4096 + 120], rax
1:
    mov rax, [rip + lr_stub_start + 4096 + 112]
    movabs rsp, 0x700000000000
    cmp rax, rsp
    jb lr_sig_linux
    lea rsp, [rip + lr_stub_start]
    cmp rax, rsp
    jb lr_sig_native
    lea rsp, [rip + lr_stub_start + 4096]
    cmp rax, rsp
    jb lr_sig_defer
lr_sig_native:
    // Native code with the native FS base: relibc's entry handles it as if it came directly.
    mov rsp, [rip + lr_stub_start + 4096 + 56]
    mov rax, [rip + lr_stub_start + 4096 + 112]
    mov [rsp + 280], rax
    mov rax, [rip + lr_stub_start + 4096 + 120]
    mov [rsp + 288], rax
    mov rax, [rip + lr_stub_start + 4096 + 96]
    mov rsp, [rip + lr_stub_start + 4096 + 104]
    jmp qword ptr [rip + lr_stub_start + 4096 + 64]

lr_sig_linux:
    LR_ENTER_FROM_LINUX
    // relibc takes the native signal (running the runtime's recorder) and then resumes at
    // lr_async_cont on this stack.
    mov rax, [rip + lr_stub_start + 4096 + 56]
    lea rdx, [rip + lr_async_cont]
    mov [rax + 280], rdx
    mov qword ptr [rax + 288], 0x202
    jmp qword ptr [rip + lr_stub_start + 4096 + 64]
lr_async_cont:
    cld
    mov rdi, rsp
    lea rsi, [rip + lr_stub_start + 4096]
    call qword ptr [rip + lr_stub_start + 4096 + 80]
    jmp lr_stub_exit

lr_sig_defer:
    // Inside this page, where FS or the stack is half switched: resume, leaving the signal
    // pending. The kernel offers it again at the next switch; a crossing in progress reaches
    // native code first and takes it there.
    mov rax, [rip + lr_stub_start + 4096 + 96]
    mov [rip + lr_stub_start + 4096 + 128], rax
    mov rax, [rip + lr_stub_start + 4096 + 104]
    mov [rip + lr_stub_start + 4096 + 136], rax
    mov rax, [rip + lr_stub_start + 4096 + 112]
    mov [rip + lr_stub_start + 4096 + 144], rax
    mov rax, [rip + lr_stub_start + 4096 + 120]
    mov [rip + lr_stub_start + 4096 + 152], rax
    mov rax, [rip + lr_stub_start + 4096 + 56]
    lock and qword ptr [rax + 272], -2
    // From here a new signal may enter lr_sig_entry, which resumes from the S slots.
lr_defer_crit:
    mov rsp, [rip + lr_stub_start + 4096 + 136]
    lea rsp, [rsp - 136]
    push qword ptr [rip + lr_stub_start + 4096 + 152]
    popfq
    lea rsp, [rsp + 136]
    mov rax, [rip + lr_stub_start + 4096 + 128]
    jmp qword ptr [rip + lr_stub_start + 4096 + 144]
lr_defer_end:

    .globl lr_excp_entry
lr_excp_entry:
    LR_SAVE_INTERRUPTED
    mov rax, [rip + lr_stub_start + 4096 + 112]
    movabs rsp, 0x700000000000
    cmp rax, rsp
    jb 1f
    // A fault in native code (the runtime, relibc, this page): relibc's entry, as before.
    mov rax, [rip + lr_stub_start + 4096 + 96]
    mov rsp, [rip + lr_stub_start + 4096 + 104]
    jmp qword ptr [rip + lr_stub_start + 4096 + 72]
1:
    LR_ENTER_FROM_LINUX
    cld
    mov rdi, rsp
    lea rsi, [rip + lr_stub_start + 4096]
    call qword ptr [rip + lr_stub_start + 4096 + 88]
    jmp lr_stub_exit

    .globl lr_stub_end
lr_stub_end:
    .balign 4096
    // Two pages that the template's RIP-relative operands name; never used themselves.
    .skip 8192
    .text
