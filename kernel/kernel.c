#include <stdint.h>
#include "drivers/vga.h"
#include "lib/printf.h"

/* Multiboot magic number */
#define MULTIBOOT_MAGIC 0x2BADB002

/* Kernel entry point - called from boot.asm */
void kernel_main(uint32_t magic, uint32_t *mboot_info) {
    /* Initialize VGA text mode */
    vga_init();

    /* Set a nice color scheme */
    vga_set_color(vga_entry_color(VGA_COLOR_LIGHT_GREEN, VGA_COLOR_BLACK));

    /* Print welcome banner */
    vga_println("================================================================================");
    vga_println("                     Welcome to Mini Educational Kernel!");
    vga_println("================================================================================");
    vga_println("");

    /* Reset to normal color */
    vga_set_color(vga_entry_color(VGA_COLOR_LIGHT_GREY, VGA_COLOR_BLACK));

    /* Verify multiboot */
    if (magic == MULTIBOOT_MAGIC) {
        kprintf("Multiboot: OK (magic = 0x%x)\n", magic);
        kprintf("Multiboot info at: %p\n", mboot_info);
    } else {
        vga_set_color(vga_entry_color(VGA_COLOR_LIGHT_RED, VGA_COLOR_BLACK));
        kprintf("Warning: Invalid multiboot magic (0x%x)\n", magic);
        vga_set_color(vga_entry_color(VGA_COLOR_LIGHT_GREY, VGA_COLOR_BLACK));
    }

    vga_println("");
    vga_println("Kernel initialized successfully!");
    vga_println("");

    /* Halt here for now */
    vga_set_color(vga_entry_color(VGA_COLOR_CYAN, VGA_COLOR_BLACK));
    vga_println("System halted. More features coming soon...");

    /* Halt the CPU */
    for (;;) {
        __asm__ volatile("hlt");
    }
}
