.text
.global _start

_start:
    // Linux ARM64 64-byte Boot Header
    b kernel_entry               // 0x00: code0 (branch to entry)
    .word 0                      // 0x04: code1
    .quad 0x80000                // 0x08: text_offset (512 KB)
    .quad 0x200000               // 0x10: image_size (2 MB)
    .quad 0x0a                   // 0x18: flags (Little-endian, 4KB page size)
    .quad 0                      // 0x20: res2
    .quad 0                      // 0x28: res3
    .quad 0                      // 0x30: res4
    .ascii "ARM\x64"             // 0x38: magic (0x644d5241)
    .word 0                      // 0x3c: res5

.align 3
kernel_entry:
    // x19 = UART base address (0x09000000)
    movz x19, #0x0900, lsl #16

    // Print welcome banner
    adr x20, msg_welcome
1:
    ldrb w21, [x20], #1
    cbz w21, 2f
    strb w21, [x19]
    b 1b

2:
    // Verify x0 points to valid DTB (magic in BE is 0xd00dfeed -> 0xedfe0dd0 in LE)
    ldr w22, [x0]
    movz w23, #0x0dd0
    movk w23, #0xedfe, lsl #16
    cmp w22, w23
    b.ne dtb_fail

    // DTB magic verified
    adr x20, msg_dtb_ok
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

dtb_fail:
    adr x20, msg_dtb_fail
5:
    ldrb w21, [x20], #1
    cbz w21, 6f
    strb w21, [x19]
    b 5b

6:
    wfi
    b .

.align 4
msg_welcome:
    .asciz "\n========================================\n[Linux Guest] Hello from Linux ARM64 Boot Protocol payload!\n[Linux Guest] Executing via 64-byte Image header at text_offset 0x80000\n========================================\n"
msg_dtb_ok:
    .asciz "[Linux Guest] DTB pointer in x0 verified (magic: 0xd00dfeed): PASSED!\n[Linux Guest] Requesting clean shutdown via PSCI SYSTEM_OFF...\n"
msg_dtb_fail:
    .asciz "[Linux Guest] DTB pointer in x0 verification: FAILED!\n"
