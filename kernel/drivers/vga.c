#include "vga.h"
#include "../cpu/ports.h"
#include "../lib/string.h"

/* VGA text buffer address */
static uint16_t *const VGA_BUFFER = (uint16_t *)0xB8000;

/* VGA I/O ports */
#define VGA_CTRL_REG 0x3D4
#define VGA_DATA_REG 0x3D5

/* Current cursor position */
static size_t cursor_row;
static size_t cursor_col;

/* Current color attribute */
static uint8_t current_color;

/* Scroll the screen up by one line */
static void vga_scroll(void) {
    /* Move all lines up by one */
    for (size_t y = 0; y < VGA_HEIGHT - 1; y++) {
        for (size_t x = 0; x < VGA_WIDTH; x++) {
            VGA_BUFFER[y * VGA_WIDTH + x] = VGA_BUFFER[(y + 1) * VGA_WIDTH + x];
        }
    }

    /* Clear the last line */
    uint16_t blank = vga_entry(' ', current_color);
    for (size_t x = 0; x < VGA_WIDTH; x++) {
        VGA_BUFFER[(VGA_HEIGHT - 1) * VGA_WIDTH + x] = blank;
    }
}

void vga_init(void) {
    cursor_row = 0;
    cursor_col = 0;
    current_color = vga_entry_color(VGA_COLOR_LIGHT_GREY, VGA_COLOR_BLACK);
    vga_clear();
    vga_enable_cursor(14, 15);
}

void vga_clear(void) {
    uint16_t blank = vga_entry(' ', current_color);
    for (size_t y = 0; y < VGA_HEIGHT; y++) {
        for (size_t x = 0; x < VGA_WIDTH; x++) {
            VGA_BUFFER[y * VGA_WIDTH + x] = blank;
        }
    }
    cursor_row = 0;
    cursor_col = 0;
    vga_update_cursor();
}

void vga_set_color(uint8_t color) {
    current_color = color;
}

void vga_putchar(char c) {
    if (c == '\n') {
        cursor_col = 0;
        cursor_row++;
    } else if (c == '\r') {
        cursor_col = 0;
    } else if (c == '\t') {
        /* Tab to next 8-column boundary */
        cursor_col = (cursor_col + 8) & ~7;
        if (cursor_col >= VGA_WIDTH) {
            cursor_col = 0;
            cursor_row++;
        }
    } else if (c == '\b') {
        /* Backspace */
        if (cursor_col > 0) {
            cursor_col--;
            VGA_BUFFER[cursor_row * VGA_WIDTH + cursor_col] = vga_entry(' ', current_color);
        }
    } else {
        VGA_BUFFER[cursor_row * VGA_WIDTH + cursor_col] = vga_entry(c, current_color);
        cursor_col++;
        if (cursor_col >= VGA_WIDTH) {
            cursor_col = 0;
            cursor_row++;
        }
    }

    /* Scroll if needed */
    while (cursor_row >= VGA_HEIGHT) {
        vga_scroll();
        cursor_row--;
    }

    vga_update_cursor();
}

void vga_print(const char *str) {
    while (*str) {
        vga_putchar(*str++);
    }
}

void vga_println(const char *str) {
    vga_print(str);
    vga_putchar('\n');
}

void vga_set_cursor(size_t x, size_t y) {
    if (x < VGA_WIDTH && y < VGA_HEIGHT) {
        cursor_col = x;
        cursor_row = y;
        vga_update_cursor();
    }
}

size_t vga_get_cursor_x(void) {
    return cursor_col;
}

size_t vga_get_cursor_y(void) {
    return cursor_row;
}

void vga_enable_cursor(uint8_t cursor_start, uint8_t cursor_end) {
    outb(VGA_CTRL_REG, 0x0A);
    outb(VGA_DATA_REG, (inb(VGA_DATA_REG) & 0xC0) | cursor_start);
    outb(VGA_CTRL_REG, 0x0B);
    outb(VGA_DATA_REG, (inb(VGA_DATA_REG) & 0xE0) | cursor_end);
}

void vga_disable_cursor(void) {
    outb(VGA_CTRL_REG, 0x0A);
    outb(VGA_DATA_REG, 0x20);
}

void vga_update_cursor(void) {
    uint16_t pos = cursor_row * VGA_WIDTH + cursor_col;
    outb(VGA_CTRL_REG, 0x0F);
    outb(VGA_DATA_REG, (uint8_t)(pos & 0xFF));
    outb(VGA_CTRL_REG, 0x0E);
    outb(VGA_DATA_REG, (uint8_t)((pos >> 8) & 0xFF));
}
