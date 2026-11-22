#!/usr/bin/env python3
import os
import struct
import subprocess
import tempfile

os.makedirs('build/esp', exist_ok=True)
target_path = 'build/esp/SH.ELF'

assembly_src = """
.text
.globl _start
.balign 4

_start:
    // Reserve 512 bytes on user stack
    sub     sp, sp, #512
    stp     x19, x20, [sp, #384]
    stp     x21, x22, [sp, #400]
    stp     x29, x30, [sp, #416]

    // Save argc (x0) and argv (x1)
    str     x0, [sp, #0]
    str     x1, [sp, #8]
    mov     x19, x0
    mov     x20, x1

    // If argc > 0, print arguments
    cbz     x19, 1f

    adr     x0, msg_argc_prefix
    bl      print_str
    mov     x0, x19
    add     x1, sp, #32
    bl      format_u64_dec
    add     x0, sp, #32
    bl      print_str
    adr     x0, msg_crlf
    bl      print_str

    mov     x21, #0
2:  cmp     x21, x19
    b.ge    1f

    adr     x0, msg_argv_prefix
    bl      print_str
    mov     x0, x21
    add     x1, sp, #32
    bl      format_u64_dec
    add     x0, sp, #32
    bl      print_str
    adr     x0, msg_argv_mid
    bl      print_str

    ldr     x0, [x20, x21, lsl #3]
    bl      print_str

    adr     x0, msg_argv_suffix
    bl      print_str

    add     x21, x21, #1
    b       2b

1:
repl_prompt:
    adr     x0, msg_prompt
    bl      print_str

    mov     x21, #0
read_char_loop:
    mov     x8, #63
    mov     x0, #0
    add     x1, sp, #16
    mov     x2, #1
    svc     #0

    cmp     x0, #1
    b.ne    read_char_loop

    ldrb    w22, [sp, #16]

    cmp     w22, #13
    b.eq    line_done
    cmp     w22, #10
    b.eq    line_done

    cmp     w22, #8
    b.eq    handle_bs
    cmp     w22, #127
    b.eq    handle_bs

    cmp     w22, #32
    b.lt    read_char_loop
    cmp     w22, #126
    b.gt    read_char_loop

    cmp     x21, #250
    b.ge    read_char_loop

    add     x2, sp, #64
    strb    w22, [x2, x21]
    add     x21, x21, #1

    mov     x8, #64
    mov     x0, #1
    add     x1, sp, #16
    mov     x2, #1
    svc     #0
    b       read_char_loop

handle_bs:
    cbz     x21, read_char_loop
    sub     x21, x21, #1
    adr     x0, msg_bs
    bl      print_str
    b       read_char_loop

line_done:
    adr     x0, msg_crlf
    bl      print_str

    add     x2, sp, #64
    strb    wzr, [x2, x21]

    add     x19, sp, #64
trim_spaces:
    ldrb    w0, [x19]
    cmp     w0, #' '
    b.ne    check_empty
    add     x19, x19, #1
    b       trim_spaces

check_empty:
    cbz     w0, repl_prompt

    mov     x0, x19
    adr     x1, str_exit
    bl      str_eq
    cbnz    w0, do_exit

    mov     x0, x19
    adr     x1, str_help
    bl      str_eq
    cbnz    w0, do_help

    mov     x0, x19
    adr     x1, str_pid
    bl      str_eq
    cbnz    w0, do_pid

    mov     x0, x19
    adr     x1, str_uptime
    bl      str_eq
    cbnz    w0, do_uptime

    mov     x0, x19
    adr     x1, str_echo
    bl      str_starts_with
    cbnz    w0, do_echo

    adr     x0, msg_unknown1
    bl      print_str
    mov     x0, x19
    bl      print_str
    adr     x0, msg_unknown2
    bl      print_str
    b       repl_prompt

do_help:
    adr     x0, msg_help
    bl      print_str
    b       repl_prompt

do_pid:
    mov     x8, #172
    svc     #0
    mov     x20, x0
    adr     x0, msg_pid_prefix
    bl      print_str
    mov     x0, x20
    add     x1, sp, #32
    bl      format_u64_dec
    add     x0, sp, #32
    bl      print_str
    adr     x0, msg_crlf
    bl      print_str
    b       repl_prompt

do_uptime:
    mov     x8, #202
    svc     #0
    mov     x20, x0
    adr     x0, msg_uptime_prefix
    bl      print_str
    mov     x0, x20
    add     x1, sp, #32
    bl      format_u64_dec
    add     x0, sp, #32
    bl      print_str
    adr     x0, msg_uptime_suffix
    bl      print_str
    b       repl_prompt

do_echo:
    add     x19, x19, #4
skip_echo_spaces:
    ldrb    w0, [x19]
    cmp     w0, #' '
    b.ne    print_echo
    add     x19, x19, #1
    b       skip_echo_spaces

print_echo:
    mov     x0, x19
    bl      print_str
    adr     x0, msg_crlf
    bl      print_str
    b       repl_prompt

do_exit:
    mov     x8, #93
    mov     x0, #0
    svc     #0
hang:
    b       hang

print_str:
    stp     x29, x30, [sp, #-16]!
    mov     x1, x0
    mov     x2, #0
strlen_loop:
    ldrb    w3, [x1, x2]
    cbz     w3, strlen_done
    add     x2, x2, #1
    b       strlen_loop
strlen_done:
    cbz     x2, print_str_ret
    mov     x8, #64
    mov     x0, #1
    svc     #0
print_str_ret:
    ldp     x29, x30, [sp], #16
    ret

str_eq:
streq_loop:
    ldrb    w2, [x0], #1
    ldrb    w3, [x1], #1
    cmp     w2, w3
    b.ne    streq_diff
    cbz     w2, streq_same
    b       streq_loop
streq_same:
    mov     w0, #1
    ret
streq_diff:
    mov     w0, #0
    ret

str_starts_with:
startswith_loop:
    ldrb    w3, [x1], #1
    cbz     w3, startswith_prefix_end
    ldrb    w2, [x0], #1
    cmp     w2, w3
    b.ne    startswith_no
    b       startswith_loop
startswith_prefix_end:
    ldrb    w2, [x0]
    cbz     w2, startswith_yes
    cmp     w2, #' '
    b.eq    startswith_yes
    mov     w0, #0
    ret
startswith_yes:
    mov     w0, #1
    ret
startswith_no:
    mov     w0, #0
    ret

format_u64_dec:
    stp     x29, x30, [sp, #-64]!
    str     x19, [sp, #16]
    mov     x19, x1
    cbnz    x0, u64_nonzero
    mov     w2, #'0'
    strb    w2, [x19]
    strb    wzr, [x19, #1]
    mov     x0, #1
    ldr     x19, [sp, #16]
    ldp     x29, x30, [sp], #64
    ret

u64_nonzero:
    add     x2, sp, #32
    mov     x3, #0
    mov     x4, #10
u64_div_loop:
    udiv    x5, x0, x4
    msub    x6, x5, x4, x0
    add     w6, w6, #'0'
    strb    w6, [x2, x3]
    add     x3, x3, #1
    mov     x0, x5
    cbnz    x0, u64_div_loop

    mov     x4, #0
u64_copy_loop:
    sub     x5, x3, x4
    sub     x5, x5, #1
    ldrb    w6, [x2, x5]
    strb    w6, [x19, x4]
    add     x4, x4, #1
    cmp     x4, x3
    b.ne    u64_copy_loop

    strb    wzr, [x19, x4]
    mov     x0, x4
    ldr     x19, [sp, #16]
    ldp     x29, x30, [sp], #64
    ret

.balign 4
msg_argc_prefix:
    .byte 'a', 'r', 'g', 'c', ':', ' ', 0
msg_argv_prefix:
    .byte ' ', ' ', 'a', 'r', 'g', 'v', '[', 0
msg_argv_mid:
    .byte ']', ':', ' ', '"', 0
msg_argv_suffix:
    .byte '"', 13, 10, 0
msg_prompt:
    .byte 'e', 'l', '0', '>', ' ', 0
msg_bs:
    .byte 8, 32, 8, 0
msg_crlf:
    .byte 13, 10, 0
msg_pid_prefix:
    .byte 'P', 'I', 'D', ':', ' ', 0
msg_uptime_prefix:
    .byte 'U', 'p', 't', 'i', 'm', 'e', ':', ' ', 0
msg_uptime_suffix:
    .byte ' ', 'm', 's', 13, 10, 0
msg_unknown1:
    .byte 'U', 'n', 'k', 'n', 'o', 'w', 'n', ' ', 'c', 'o', 'm', 'm', 'a', 'n', 'd', ':', ' ', '\'', 0
msg_unknown2:
    .byte '\'', '.', ' ', 'T', 'y', 'p', 'e', ' ', '\'', 'h', 'e', 'l', 'p', '\'', ' ', 'f', 'o', 'r', ' ', 'c', 'o', 'm', 'm', 'a', 'n', 'd', 's', '.', 13, 10, 0
msg_help:
    .byte 'E', 'L', '0', ' ', 's', 'h', 'e', 'l', 'l', ' ', 'c', 'o', 'm', 'm', 'a', 'n', 'd', 's', ':', 13, 10
    .byte ' ', ' ', 'h', 'e', 'l', 'p', ' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', '-', ' ', 's', 'h', 'o', 'w', ' ', 't', 'h', 'i', 's', ' ', 'h', 'e', 'l', 'p', 13, 10
    .byte ' ', ' ', 'p', 'i', 'd', ' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', '-', ' ', 's', 'h', 'o', 'w', ' ', 'p', 'r', 'o', 'c', 'e', 's', 's', ' ', 'P', 'I', 'D', 13, 10
    .byte ' ', ' ', 'u', 'p', 't', 'i', 'm', 'e', ' ', ' ', ' ', ' ', ' ', ' ', '-', ' ', 's', 'h', 'o', 'w', ' ', 'u', 'p', 't', 'i', 'm', 'e', ' ', 'i', 'n', ' ', 'm', 's', 13, 10
    .byte ' ', ' ', 'e', 'c', 'h', 'o', ' ', '<', 't', 'e', 'x', 't', '>', ' ', '-', ' ', 'e', 'c', 'h', 'o', ' ', 't', 'e', 'x', 't', ' ', 't', 'o', ' ', 'c', 'o', 'n', 's', 'o', 'l', 'e', 13, 10
    .byte ' ', ' ', 'e', 'x', 'i', 't', ' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', '-', ' ', 'e', 'x', 'i', 't', ' ', 'b', 'a', 'c', 'k', ' ', 't', 'o', ' ', 'k', 'e', 'r', 'n', 'e', 'l', 13, 10, 0

str_exit:
    .byte 'e', 'x', 'i', 't', 0
str_help:
    .byte 'h', 'e', 'l', 'p', 0
str_pid:
    .byte 'p', 'i', 'd', 0
str_uptime:
    .byte 'u', 'p', 't', 'i', 'm', 'e', 0
str_echo:
    .byte 'e', 'c', 'h', 'o', 0
"""

with tempfile.NamedTemporaryFile('w', suffix='.s') as f:
    f.write(assembly_src)
    f.flush()
    res = subprocess.run(['clang', '-target', 'aarch64-none-elf', '-c', f.name, '-o', '/tmp/sh_build.o'], capture_output=True)
    if res.returncode != 0:
        raise RuntimeError(f"Clang compilation failed: {res.stderr.decode()}")

with open('/tmp/sh_build.o', 'rb') as f:
    obj_data = f.read()

# Parse ELF64 object
magic, cls, data_enc, ver = struct.unpack('<4sBBB', obj_data[:7])
e_shoff, = struct.unpack('<Q', obj_data[40:48])
e_shentsize, e_shnum, e_shstrndx = struct.unpack('<HHH', obj_data[58:64])

shstrtab_hdr = obj_data[e_shoff + e_shstrndx * e_shentsize : e_shoff + (e_shstrndx + 1) * e_shentsize]
shstrtab_off, shstrtab_size = struct.unpack('<QQ', shstrtab_hdr[24:40])
shstrtab = obj_data[shstrtab_off : shstrtab_off + shstrtab_size]

text_bytes = None
for i in range(e_shnum):
    sh = obj_data[e_shoff + i * e_shentsize : e_shoff + (i + 1) * e_shentsize]
    name_off, sh_type, sh_flags, sh_addr, sh_offset, sh_size = struct.unpack('<IIQQQQ', sh[:40])
    name = shstrtab[name_off:].split(b'\x00')[0].decode()
    if name == '.text':
        text_bytes = obj_data[sh_offset : sh_offset + sh_size]
        break

if not text_bytes:
    raise RuntimeError("Failed to find .text section in object file")

# Construct final SH.ELF executable
# Layout:
# 0x00..0x3F: ELF Header (64 bytes)
# 0x40..0x77: Program Header (56 bytes)
# 0x78..0x7F: Padding (8 bytes)
# 0x80..    : Code (.text)
pad_len = 128 - 64 - 56
total_filesz = 128 + len(text_bytes)
total_memsz = total_filesz

ident = b'\x7fELF' + bytes([2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0])
ehdr = struct.pack(
    '<16sHHIQQQIHHHHHH',
    ident,
    2,              # ET_EXEC
    183,            # EM_AARCH64
    1,              # e_version
    0x0040_0080,    # e_entry
    64,             # e_phoff
    0,              # e_shoff
    0,              # e_flags
    64,             # e_ehsize
    56,             # e_phentsize
    1,              # e_phnum
    64, 0, 0
)

phdr = struct.pack(
    '<IIQQQQQQ',
    1,              # PT_LOAD
    5,              # PF_R | PF_X
    0,              # p_offset
    0x0040_0000,    # p_vaddr
    0x0040_0000,    # p_paddr
    total_filesz,   # p_filesz
    total_memsz,    # p_memsz
    4096            # p_align
)

elf_bytes = ehdr + phdr + (b'\x00' * pad_len) + text_bytes

with open(target_path, 'wb') as f:
    f.write(elf_bytes)

print(f"Generated {target_path} (size={len(elf_bytes)} bytes, entry=0x00400080)")
