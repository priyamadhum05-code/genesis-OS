#!/usr/bin/env python3
import os
import struct

os.makedirs('build/esp', exist_ok=True)

# 1. HELLO.ELF
hello_path = 'build/esp/HELLO.ELF'
if not os.path.exists(hello_path):
    magic = b'\x7fELF'
    ident = magic + bytes([2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0])
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
        256,            # p_filesz
        256,            # p_memsz
        4096            # p_align
    )
    code = bytes([
        0x28, 0x00, 0x80, 0xd2, # mov x8, #1 (SYS_WRITE)
        0xc0, 0x00, 0x00, 0x10, # adr x0, #24
        0x81, 0x04, 0x80, 0xd2, # mov x1, #36
        0x01, 0x00, 0x00, 0xd4, # svc #0
        0xa8, 0x0b, 0x80, 0xd2, # mov x8, #93 (SYS_EXIT)
        0x00, 0x00, 0x80, 0xd2, # mov x0, #0
        0x01, 0x00, 0x00, 0xd4, # svc #0
    ])
    msg = b'GENESIS: el0 user-space syscall ok\n'
    pad = b'\x00' * (128 - len(ehdr) - len(phdr))
    body = ehdr + phdr + pad + code + msg
    body += b'\x00' * (256 - len(body))
    with open(hello_path, 'wb') as f:
        f.write(body)
    print('Generated', hello_path)

# 2. MODEL.BIN
model_path = 'build/esp/MODEL.BIN'
if not os.path.exists(model_path):
    hdr = struct.pack('<8sIIII', b'GENESNN1', 64, 64, 10, 0)
    inp_vals = [((i * 3) % 15) - 7 for i in range(64)]
    w1_vals = [(((i + j) * 5) % 11) - 5 for i in range(64) for j in range(64)]
    b1_vals = [((i * 13) % 50) - 20 for i in range(64)]
    w2_vals = [(((k * 7 + i) * 3) % 13) - 6 for k in range(10) for i in range(64)]
    b2_vals = [(1000 if k == 3 else 0) for k in range(10)]

    inp = struct.pack('<64b', *inp_vals)
    w1 = struct.pack('<4096b', *w1_vals)
    b1 = struct.pack('<64i', *b1_vals)
    w2 = struct.pack('<640b', *w2_vals)
    b2 = struct.pack('<10i', *b2_vals)

    model_data = hdr + inp + w1 + b1 + w2 + b2
    with open(model_path, 'wb') as f:
        f.write(model_data)
    print('Generated', model_path)

# 3. TRANSFORMER.BIN
transformer_path = 'build/esp/TRANSFORMER.BIN'
if not os.path.exists(transformer_path):
    hdr = struct.pack('<8sIIII', b'GENESTF1', 16, 64, 128, 0) + b'\x00' * 8
    inp_vals = [((t * 5 + d * 3) % 19) - 9 for t in range(16) for d in range(64)]
    wq_vals = [(((d + i) * 7) % 13) - 6 for d in range(64) for i in range(64)]
    bq_vals = [((d * 11) % 40) - 20 for d in range(64)]
    wk_vals = [(((d * 3 + i * 5) % 11) - 5) for d in range(64) for i in range(64)]
    bk_vals = [((d * 7) % 30) - 15 for d in range(64)]
    wv_vals = [(((d * 2 + i * 7) % 17) - 8) for d in range(64) for i in range(64)]
    bv_vals = [((d * 5) % 25) - 12 for d in range(64)]
    w_ff1_vals = [(((h + i) * 3) % 15) - 7 for h in range(128) for i in range(64)]
    b_ff1_vals = [((h * 9) % 50) - 25 for h in range(128)]
    w_ff2_vals = [(((d + h * 3) % 11) - 5) for d in range(64) for h in range(128)]
    b_ff2_vals = [((d * 13) % 35) - 17 for d in range(64)]

    inp = struct.pack('<1024b', *inp_vals)
    wq = struct.pack('<4096b', *wq_vals)
    bq = struct.pack('<64i', *bq_vals)
    wk = struct.pack('<4096b', *wk_vals)
    bk = struct.pack('<64i', *bk_vals)
    wv = struct.pack('<4096b', *wv_vals)
    bv = struct.pack('<64i', *bv_vals)
    w_ff1 = struct.pack('<8192b', *w_ff1_vals)
    b_ff1 = struct.pack('<128i', *b_ff1_vals)
    w_ff2 = struct.pack('<8192b', *w_ff2_vals)
    b_ff2 = struct.pack('<64i', *b_ff2_vals)

    tf_data = hdr + inp + wq + bq + wk + bk + wv + bv + w_ff1 + b_ff1 + w_ff2 + b_ff2
    pad = (512 - (len(tf_data) % 512)) % 512
    tf_data += b'\x00' * pad
    with open(transformer_path, 'wb') as f:
        f.write(tf_data)
    print('Generated', transformer_path)
