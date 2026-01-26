/* Linker script for the kernel */

ENTRY(_start)

SECTIONS
{
    /* Load at 1MB - standard location for kernels */
    . = 1M;

    /* Multiboot header must come first */
    .multiboot ALIGN(4K) :
    {
        *(.multiboot)
    }

    /* Text section (code) */
    .text ALIGN(4K) :
    {
        *(.text)
    }

    /* Read-only data */
    .rodata ALIGN(4K) :
    {
        *(.rodata)
    }

    /* Initialized data */
    .data ALIGN(4K) :
    {
        *(.data)
    }

    /* Uninitialized data (BSS) */
    .bss ALIGN(4K) :
    {
        *(COMMON)
        *(.bss)
    }

    /* End of kernel marker */
    _kernel_end = .;
}
