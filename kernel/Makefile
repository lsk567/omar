# Minimal Educational Kernel Makefile

# Compilers and tools
CC = gcc
AS = nasm
LD = ld

# Directories
BOOT_DIR = boot
CPU_DIR = cpu
DRIVERS_DIR = drivers
LIB_DIR = lib
MM_DIR = mm
SHELL_DIR = shell
BUILD_DIR = build
ISO_DIR = isodir

# Flags
CFLAGS = -m32 -ffreestanding -fno-stack-protector -fno-pie -nostdlib \
         -Wall -Wextra -O2 -g
ASFLAGS = -f elf32
LDFLAGS = -m elf_i386 -T linker.ld -nostdlib

# Source files
ASM_SOURCES = $(BOOT_DIR)/boot.asm
C_SOURCES = kernel.c \
            $(DRIVERS_DIR)/vga.c \
            $(LIB_DIR)/string.c \
            $(LIB_DIR)/printf.c

# Object files
ASM_OBJECTS = $(patsubst %.asm,$(BUILD_DIR)/%.o,$(ASM_SOURCES))
C_OBJECTS = $(patsubst %.c,$(BUILD_DIR)/%.o,$(C_SOURCES))
OBJECTS = $(ASM_OBJECTS) $(C_OBJECTS)

# Output
KERNEL = $(BUILD_DIR)/kernel.bin
ISO = $(BUILD_DIR)/kernel.iso

# Default target
all: $(ISO)

# Create build directories
$(BUILD_DIR):
	mkdir -p $(BUILD_DIR)/$(BOOT_DIR)
	mkdir -p $(BUILD_DIR)/$(CPU_DIR)
	mkdir -p $(BUILD_DIR)/$(DRIVERS_DIR)
	mkdir -p $(BUILD_DIR)/$(LIB_DIR)
	mkdir -p $(BUILD_DIR)/$(MM_DIR)
	mkdir -p $(BUILD_DIR)/$(SHELL_DIR)

# Compile assembly files
$(BUILD_DIR)/%.o: %.asm | $(BUILD_DIR)
	$(AS) $(ASFLAGS) $< -o $@

# Compile C files
$(BUILD_DIR)/%.o: %.c | $(BUILD_DIR)
	$(CC) $(CFLAGS) -c $< -o $@

# Link the kernel
$(KERNEL): $(OBJECTS)
	$(LD) $(LDFLAGS) $(OBJECTS) -o $@

# Create ISO image
$(ISO): $(KERNEL)
	mkdir -p $(ISO_DIR)/boot/grub
	cp $(KERNEL) $(ISO_DIR)/boot/kernel.bin
	echo 'set timeout=0' > $(ISO_DIR)/boot/grub/grub.cfg
	echo 'set default=0' >> $(ISO_DIR)/boot/grub/grub.cfg
	echo '' >> $(ISO_DIR)/boot/grub/grub.cfg
	echo 'menuentry "Mini Educational Kernel" {' >> $(ISO_DIR)/boot/grub/grub.cfg
	echo '    multiboot /boot/kernel.bin' >> $(ISO_DIR)/boot/grub/grub.cfg
	echo '    boot' >> $(ISO_DIR)/boot/grub/grub.cfg
	echo '}' >> $(ISO_DIR)/boot/grub/grub.cfg
	grub-mkrescue -o $@ $(ISO_DIR) 2>/dev/null

# Run in QEMU
run: $(ISO)
	qemu-system-i386 -cdrom $(ISO) -m 32M

# Run with debug output
debug: $(ISO)
	qemu-system-i386 -cdrom $(ISO) -m 32M -d int -no-reboot

# Clean build artifacts
clean:
	rm -rf $(BUILD_DIR) $(ISO_DIR)

# Phony targets
.PHONY: all run debug clean
