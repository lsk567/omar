; Multiboot-compliant bootloader entry
; This file sets up the multiboot header and calls kernel_main

; Multiboot constants
MBOOT_PAGE_ALIGN    equ 1 << 0              ; Align loaded modules on page boundaries
MBOOT_MEM_INFO      equ 1 << 1              ; Provide memory map
MBOOT_HEADER_MAGIC  equ 0x1BADB002          ; Multiboot magic number
MBOOT_HEADER_FLAGS  equ MBOOT_PAGE_ALIGN | MBOOT_MEM_INFO
MBOOT_CHECKSUM      equ -(MBOOT_HEADER_MAGIC + MBOOT_HEADER_FLAGS)

; Kernel stack size (16KB)
KERNEL_STACK_SIZE   equ 16384

section .multiboot
align 4
    dd MBOOT_HEADER_MAGIC
    dd MBOOT_HEADER_FLAGS
    dd MBOOT_CHECKSUM

section .bss
align 16
stack_bottom:
    resb KERNEL_STACK_SIZE
stack_top:

section .text
global _start
extern kernel_main

_start:
    ; Set up the stack
    mov esp, stack_top

    ; Push multiboot info pointer and magic number for kernel_main
    push ebx                    ; Multiboot info structure pointer
    push eax                    ; Multiboot magic number

    ; Call the kernel main function
    call kernel_main

    ; If kernel_main returns, halt the CPU
.hang:
    cli
    hlt
    jmp .hang
