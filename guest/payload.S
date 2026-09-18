.text
.global _start
_start:
    // x19 = UART base address (0x09000000)
    movz x19, #0x0900, lsl #16

    // Print welcome banner
    adr x20, msg
1:
    ldrb w21, [x20], #1
    cbz w21, 2f
    strb w21, [x19]
    b 1b

2:
    // Memory read/write test via stack (SP_EL1)
    movz x0, #0xbabe
    movk x0, #0xcafe, lsl #16
    movk x0, #0xbeef, lsl #32
    movk x0, #0xdead, lsl #48
    str x0, [sp, #-16]!
    ldr x1, [sp], #16
    cmp x0, x1
    b.ne 5f

    // Memory test passed
    adr x20, pass_msg
3:
    ldrb w21, [x20], #1
    cbz w21, 4f
    strb w21, [x19]
    b 3b

4:
    // Call PSCI SYSTEM_OFF via HVC #0: function ID 0x84000008
    movz x0, #0x0008
    movk x0, #0x8400, lsl #16
    hvc #0
    b .

5:
    // Memory test failed
    adr x20, fail_msg
6:
    ldrb w21, [x20], #1
    cbz w21, 7f
    strb w21, [x19]
    b 6b

7:
    wfi
    b .

.align 4
msg:
    .asciz "\n========================================\n[Guest] Hello from Apple Silicon ARM64 VM!\n[Guest] Executing at EL1 via Hypervisor.framework\n========================================\n"
pass_msg:
    .asciz "[Guest] Guest RAM Read/Write Test: PASSED!\n[Guest] Requesting clean shutdown via PSCI SYSTEM_OFF (HVC)...\n"
fail_msg:
    .asciz "[Guest] Guest RAM Read/Write Test: FAILED!\n"
