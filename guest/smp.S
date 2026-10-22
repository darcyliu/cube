# BSP stays the long-mode image at GPA 0x100000. SIPI can only start below 1 MiB,
# so the BSP copies the real-mode trampoline to 0x70000 (vector 0x70).
.code64
.section .text
.globl _start
_start:
    lea handler(%rip), %rax
    mov $0x30300, %rdi
    mov %ax, (%rdi)
    movw $8, 2(%rdi)
    movb $0x8e, 5(%rdi)
    shr $16, %rax
    mov %ax, 6(%rdi)
    shr $16, %rax
    mov %eax, 8(%rdi)

    lidt idtr(%rip)

    mov $0xfee000f0, %rdx
    movl $0x1ff, (%rdx)

    mov $0xfec00000, %rdx
    movl $0x19, (%rdx)
    mov $0xfec00010, %rdx
    movl $0, (%rdx)
    mov $0xfec00000, %rdx
    movl $0x18, (%rdx)
    mov $0xfec00010, %rdx
    movl $0x8030, (%rdx)

    lea tramp(%rip), %rsi
    mov $0x70000, %rdi
    mov $(tramp_end - tramp), %rcx
    rep movsb

    mov $0x3f9, %dx
    mov $1, %al
    outb %al, %dx

    sti
    nop
1:
    hlt
    jmp 1b

handler:
    mov $0x3f8, %dx
    inb %dx, %al
    outb %al, %dx
    mov $0xfee000b0, %rdx
    movl $0, (%rdx)
    mov $0xfee00310, %rdx
    movl $0x01000000, (%rdx)
    mov $0xfee00300, %rdx
    movl $0x500, (%rdx)
    movl $0x670, (%rdx)
    iretq

idtr:
    .word 0x0fff
    .quad 0x30000

# Raw bytes: mov $0xa5,%al; mov $0x402,%dx; out %al,%dx; hlt; jmp .
tramp:
    .byte 0xb0, 0xa5
    .byte 0xba, 0x02, 0x04
    .byte 0xee
    .byte 0xf4
    .byte 0xeb, 0xfe
tramp_end:
