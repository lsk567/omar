#include "printf.h"
#include "string.h"
#include "../drivers/vga.h"

static void print_num(uint32_t num, int base, int is_signed, int width, char pad) {
    char buf[32];
    int i = 0;
    int negative = 0;

    if (is_signed && (int32_t)num < 0) {
        negative = 1;
        num = -(int32_t)num;
    }

    if (num == 0) {
        buf[i++] = '0';
    } else {
        while (num > 0) {
            int digit = num % base;
            buf[i++] = digit < 10 ? '0' + digit : 'a' + digit - 10;
            num /= base;
        }
    }

    if (negative) {
        buf[i++] = '-';
    }

    /* Pad if needed */
    while (i < width) {
        buf[i++] = pad;
    }

    /* Print in reverse */
    while (i > 0) {
        vga_putchar(buf[--i]);
    }
}

int kprintf(const char *format, ...) {
    va_list args;
    va_start(args, format);

    int count = 0;
    char c;

    while ((c = *format++) != '\0') {
        if (c != '%') {
            vga_putchar(c);
            count++;
            continue;
        }

        /* Parse format specifier */
        int width = 0;
        char pad = ' ';

        c = *format++;
        if (c == '\0') break;

        /* Check for zero padding */
        if (c == '0') {
            pad = '0';
            c = *format++;
            if (c == '\0') break;
        }

        /* Parse width */
        while (c >= '0' && c <= '9') {
            width = width * 10 + (c - '0');
            c = *format++;
            if (c == '\0') break;
        }

        switch (c) {
            case 'd':
            case 'i': {
                int val = va_arg(args, int);
                print_num((uint32_t)val, 10, 1, width, pad);
                count++;
                break;
            }
            case 'u': {
                uint32_t val = va_arg(args, uint32_t);
                print_num(val, 10, 0, width, pad);
                count++;
                break;
            }
            case 'x': {
                uint32_t val = va_arg(args, uint32_t);
                print_num(val, 16, 0, width, pad);
                count++;
                break;
            }
            case 'p': {
                vga_print("0x");
                uint32_t val = (uint32_t)va_arg(args, void *);
                print_num(val, 16, 0, 8, '0');
                count++;
                break;
            }
            case 's': {
                const char *str = va_arg(args, const char *);
                if (str == NULL) str = "(null)";
                while (*str) {
                    vga_putchar(*str++);
                    count++;
                }
                break;
            }
            case 'c': {
                char ch = (char)va_arg(args, int);
                vga_putchar(ch);
                count++;
                break;
            }
            case '%':
                vga_putchar('%');
                count++;
                break;
            default:
                vga_putchar('%');
                vga_putchar(c);
                count += 2;
                break;
        }
    }

    va_end(args);
    return count;
}
