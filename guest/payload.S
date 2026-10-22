# Copied to GPA 0x100000, not linked there. Addresses are RIP-relative.
.code64
.section .text
.globl _start
_start:
    lea banner(%rip), %rsi
    mov $0x3f8, %dx
1:
    movzbl (%rsi), %eax
    test %al, %al
    jz 2f
    out %al, %dx
    inc %rsi
    jmp 1b
2:
    hlt
    jmp 2b
banner:
    .asciz "[vmm-intel] long mode guest on COM1\n"
