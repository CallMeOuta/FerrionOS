# Why Rust?

Why is the system written in Rust?

For an operating system, security and stability matter more than for most software, because every other program depends on it.

Linux, the BSDs, glibc, GNU Bash, X11 and many other system programs have had numerous bugs and security vulnerabilities that come from the lack of memory safety and type safety. Rust enforces memory safety and type safety statically, at compile time.

Design matters, and so does implementation. A design is a transparent source of issues: it is known what is going on, what was intended and what was not. Rust addresses the implementation: it avoids the unexpected unsafe memory conditions that are a major source of security-critical bugs.

The basic separation of kernel and user space is similar to that of other Unix-like systems: the kernel enforces it through a permission system with a small attack surface, and the CPU enforces the isolation of address spaces.

What Rust adds is enforced memory and type safety. A large number of unexpected bugs, such as undefined behavior, are found by the compiler.

Many bugs in Linux and the BSDs come from unsafe conditions such as buffer overflows, which safe Rust rules out, and not from their overall design.

## Unsafe Code

`unsafe` tells Rust that the programmer takes responsibility for an operation that the compiler cannot check. It is often necessary in low-level code that provides safe abstractions, and a kernel cannot be written without it.

A kernel therefore cannot be verified completely by the Rust compiler. The unsafe parts have to be marked with `unsafe`, which keeps them apart from the safe code and makes them easy to find and review. The components keep `unsafe` to the places that need it.

This contrasts with kernels written in C, which cannot make guarantees about memory safety without costly formal analysis.

You can find out more about how `unsafe` works in the [relevant section](https://doc.rust-lang.org/book/ch20-01-unsafe-rust.html) of the Rust book.

## Benefits

The following sections explain the benefits of Rust.

### Fewer bugs

The strict language rules and the compiler's checks reduce the probability of bugs.

The borrow checker verifies memory accesses at compile time, which eliminates most memory bugs in safe code, such as buffer overflows and dangling pointers.

The type system rejects invalid use of a type at compile time, and new types can be created to make invalid states impossible to express, which eliminates some logic bugs and reduces the effort of formal verification.

### Less data corruption

The Rust compiler helps the programmer avoid memory errors and race conditions, which reduces the probability of bugs that corrupt data.

### Memory safety does not replace exploit mitigations

Rust removes whole classes of memory bugs from safe code. Bugs remain possible in `unsafe` code, in the C and C++ programs and libraries that FerrionOS ports, and in program logic. Exploit mitigations limit what such a bug can be used for.

FerrionOS has no complete set of exploit mitigations today. They are planned, and [Security](./security.md) lists the present state.

### A small kernel

The kernel is small, so it uses little memory, and there is less kernel code in which a bug can hide ([KISS](https://en.wikipedia.org/wiki/KISS_principle)).

A safe and fast language, combined with a small kernel, helps to keep the core of the system reliable, fast and easy to maintain.

### Thread safety

The support for thread safety in C and C++ is fragile. It is easy to write a program that looks safe to run across multiple threads, and that still introduces subtle bugs or security holes. If one thread accesses a piece of state at the same time as another thread changes it, the whole program can show confusing bugs.

Safe Rust rules out data races: the same type system that prevents memory mistakes also prevents unsynchronized access to shared state from several threads.

It does not rule out every race condition. A [time-of-check to time-of-use](https://en.wikipedia.org/wiki/Time_of_check_to_time_of_use) error between two processes, for example, is a logic error that the compiler cannot see.

### Drivers written in Rust

Drivers written in Rust have a lower probability of memory bugs, which is good for their stability and security.

### Code quality with less effort

More verification by the compiler, and standard tooling for testing (`cargo test`), linting (Clippy) and formatting (`cargo fmt`), reduce the effort it takes to improve code quality and test coverage.
