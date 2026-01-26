#ifndef VGA_H
#define VGA_H

#include <stdint.h>
#include <stddef.h>

/* VGA text mode dimensions */
#define VGA_WIDTH  80
#define VGA_HEIGHT 25

/* VGA colors */
typedef enum {
    VGA_COLOR_BLACK = 0,
    VGA_COLOR_BLUE = 1,
    VGA_COLOR_GREEN = 2,
    VGA_COLOR_CYAN = 3,
    VGA_COLOR_RED = 4,
    VGA_COLOR_MAGENTA = 5,
    VGA_COLOR_BROWN = 6,
    VGA_COLOR_LIGHT_GREY = 7,
    VGA_COLOR_DARK_GREY = 8,
    VGA_COLOR_LIGHT_BLUE = 9,
    VGA_COLOR_LIGHT_GREEN = 10,
    VGA_COLOR_LIGHT_CYAN = 11,
    VGA_COLOR_LIGHT_RED = 12,
    VGA_COLOR_LIGHT_MAGENTA = 13,
    VGA_COLOR_LIGHT_BROWN = 14,
    VGA_COLOR_WHITE = 15,
} vga_color_t;

/* Create a VGA color byte from foreground and background colors */
static inline uint8_t vga_entry_color(vga_color_t fg, vga_color_t bg) {
    return fg | (bg << 4);
}

/* Create a VGA entry (character + color) */
static inline uint16_t vga_entry(unsigned char c, uint8_t color) {
    return (uint16_t)c | ((uint16_t)color << 8);
}

/* Initialize VGA text mode */
void vga_init(void);

/* Clear the screen */
void vga_clear(void);

/* Set the current text color */
void vga_set_color(uint8_t color);

/* Put a character at the current cursor position */
void vga_putchar(char c);

/* Print a string */
void vga_print(const char *str);

/* Print a string with a newline */
void vga_println(const char *str);

/* Set cursor position */
void vga_set_cursor(size_t x, size_t y);

/* Get current cursor X position */
size_t vga_get_cursor_x(void);

/* Get current cursor Y position */
size_t vga_get_cursor_y(void);

/* Enable/disable hardware cursor */
void vga_enable_cursor(uint8_t cursor_start, uint8_t cursor_end);
void vga_disable_cursor(void);

/* Update hardware cursor position */
void vga_update_cursor(void);

#endif /* VGA_H */
