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

# 4. APP1.ELF and APP2.ELF
def build_app_elf(pid: int, path: str):
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
        0x88, 0x15, 0x80, 0xd2, # mov x8, #172 (SYS_GETPID)
        0x01, 0x00, 0x00, 0xd4, # svc #0
        0x73, 0x00, 0x80, 0xd2, # mov x19, #3
        0x28, 0x00, 0x80, 0xd2, # mov x8, #1
        0x20, 0x02, 0x00, 0x10, # adr x0, msg1
        0x81, 0x02, 0x80, 0xd2, # mov x1, #20
        0x01, 0x00, 0x00, 0xd4, # svc #0
        0x14, 0xfa, 0x80, 0xd2, # mov x20, #2000
        0x94, 0x06, 0x00, 0xf1, # subs x20, x20, #1
        0xe1, 0xff, 0xff, 0x54, # b.ne 0x20
        0x88, 0x0f, 0x80, 0xd2, # mov x8, #124 (SYS_YIELD)
        0x01, 0x00, 0x00, 0xd4, # svc #0
        0x73, 0x06, 0x00, 0xf1, # subs x19, x19, #1
        0xc1, 0xfe, 0xff, 0x54, # b.ne 0x0c
        0x28, 0x00, 0x80, 0xd2, # mov x8, #1
        0x60, 0x01, 0x00, 0x10, # adr x0, msg2
        0xa1, 0x02, 0x80, 0xd2, # mov x1, #21
        0x01, 0x00, 0x00, 0xd4, # svc #0
        0xa8, 0x0b, 0x80, 0xd2, # mov x8, #93 (SYS_EXIT)
        0x00, 0x00, 0x80, 0xd2, # mov x0, #0
        0x01, 0x00, 0x00, 0xd4, # svc #0
    ])
    msg1 = f"[PID {pid}] task active\n".encode('ascii')
    msg2 = f"[PID {pid}] task exiting\n".encode('ascii')
    pad = b'\x00' * (128 - len(ehdr) - len(phdr))
    body = ehdr + phdr + pad + code + msg1 + msg2
    body += b'\x00' * (256 - len(body))
    with open(path, 'wb') as f:
        f.write(body)
    print('Generated', path)

app1_path = 'build/esp/APP1.ELF'
if not os.path.exists(app1_path):
    build_app_elf(1, app1_path)

app2_path = 'build/esp/APP2.ELF'
if not os.path.exists(app2_path):
    build_app_elf(2, app2_path)

# 5. MICRO_LLM.BIN
llm_path = 'build/esp/MICRO_LLM.BIN'
if not os.path.exists(llm_path):
    EXP_TABLE = [
        256, 199, 155, 121, 94, 73, 57, 45,
        35, 27, 21, 16, 13, 10, 8, 6,
        5, 4, 3, 2, 2, 1, 1, 1,
        1, 0, 0, 0, 0, 0, 0, 0,
    ]

    def clamp(val, low, high):
        return max(low, min(high, val))

    def dot64(a, b, bias=0):
        s = bias
        for i in range(64):
            s += a[i] * b[i]
        return s

    def transformer_block(inp, wq, bq, wk, bk, wv, bv, w_ff1, b_ff1, w_ff2, b_ff2):
        q = [[0]*64 for _ in range(16)]
        k = [[0]*64 for _ in range(16)]
        v = [[0]*64 for _ in range(16)]
        for t in range(16):
            for d in range(64):
                q[t][d] = clamp(dot64(inp[t], wq[d], bq[d]) >> 7, -128, 127)
                k[t][d] = clamp(dot64(inp[t], wk[d], bk[d]) >> 7, -128, 127)
                v[t][d] = clamp(dot64(inp[t], wv[d], bv[d]) >> 7, -128, 127)
        out = [[0]*64 for _ in range(16)]
        for t in range(16):
            raw_scores = [dot64(q[t], k[s]) // 8 for s in range(16)]
            max_s = max(raw_scores)
            weights = [EXP_TABLE[clamp(max_s - raw_scores[s], 0, 31)] for s in range(16)]
            sum_w = sum(weights) or 1
            probs = [(weights[s] * 128) // sum_w for s in range(16)]
            x1 = [0]*64
            for d in range(64):
                acc = sum(probs[s] * v[s][d] for s in range(16))
                attn_val = clamp(acc // 128, -128, 127)
                x1[d] = clamp(inp[t][d] + attn_val, -128, 127)
            h_ffn = [0]*128
            for h in range(128):
                acc = dot64(x1, w_ff1[h], b_ff1[h])
                h_ffn[h] = clamp(acc >> 7, 0, 127) if acc > 0 else 0
            for d in range(64):
                acc1 = dot64(h_ffn[:64], w_ff2[d][:64], b_ff2[d])
                acc = dot64(h_ffn[64:], w_ff2[d][64:], acc1)
                ffn_out = clamp(acc >> 7, -128, 127)
                out[t][d] = clamp(x1[d] + ffn_out, -128, 127)
        return out

    token_emb = [[(((tok * 7 + d * 3) % 19) - 9) for d in range(64)] for tok in range(128)]
    pos_emb = [[(((t * 11 + d * 5) % 17) - 8) for d in range(64)] for t in range(16)]

    def make_block(seed):
        wq = [[(((d + i + seed) % 9) - 4) for i in range(64)] for d in range(64)]
        bq = [0]*64
        wk = [[(((d * 3 + i + seed) % 7) - 3) for i in range(64)] for d in range(64)]
        bk = [0]*64
        wv = [[(((d + i * 2 + seed) % 9) - 4) for i in range(64)] for d in range(64)]
        bv = [0]*64
        w_ff1 = [[(((h + i + seed) % 7) - 3) for i in range(64)] for h in range(128)]
        b_ff1 = [0]*128
        w_ff2 = [[(((d + h + seed) % 7) - 3) for h in range(128)] for d in range(64)]
        b_ff2 = [0]*64
        return (wq, bq, wk, bk, wv, bv, w_ff1, b_ff1, w_ff2, b_ff2)

    blk0 = make_block(1)
    blk1 = make_block(2)

    w_lm = [[0]*64 for _ in range(128)]
    b_lm = [0]*128

    prompt = list(b"GENESIS ")
    target = list(b"IS READY")

    for step in range(len(target)):
        cur_tokens = prompt + target[:step]
        n = len(cur_tokens)
        inp = [[0]*64 for _ in range(16)]
        for t in range(n):
            tok = cur_tokens[t] % 128
            for d in range(64):
                inp[t][d] = clamp(token_emb[tok][d] + pos_emb[t][d], -128, 127)
        out0 = transformer_block(inp, *blk0)
        out1 = transformer_block(out0, *blk1)
        last_h = out1[n - 1]
        desired_tok = target[step]
        for d in range(64):
            w_lm[desired_tok][d] = clamp(last_h[d], -127, 127)
        b_lm[desired_tok] = 1000

    hdr = struct.pack('<8sIIII', b'GENESLLM', 128, 64, 2, 16) + b'\x00' * 8
    token_emb_bytes = struct.pack('<8192b', *[token_emb[tok][d] for tok in range(128) for d in range(64)])
    pos_emb_bytes = struct.pack('<1024b', *[pos_emb[t][d] for t in range(16) for d in range(64)])

    def pack_block(blk):
        wq, bq, wk, bk, wv, bv, w_ff1, b_ff1, w_ff2, b_ff2 = blk
        b_data = struct.pack('<4096b', *[wq[d][i] for d in range(64) for i in range(64)])
        b_data += struct.pack('<64i', *bq)
        b_data += struct.pack('<4096b', *[wk[d][i] for d in range(64) for i in range(64)])
        b_data += struct.pack('<64i', *bk)
        b_data += struct.pack('<4096b', *[wv[d][i] for d in range(64) for i in range(64)])
        b_data += struct.pack('<64i', *bv)
        b_data += struct.pack('<8192b', *[w_ff1[h][i] for h in range(128) for i in range(64)])
        b_data += struct.pack('<128i', *b_ff1)
        b_data += struct.pack('<8192b', *[w_ff2[d][h] for d in range(64) for h in range(128)])
        b_data += struct.pack('<64i', *b_ff2)
        return b_data

    blk0_bytes = pack_block(blk0)
    blk1_bytes = pack_block(blk1)
    w_lm_bytes = struct.pack('<8192b', *[w_lm[v][d] for v in range(128) for d in range(64)])
    b_lm_bytes = struct.pack('<128i', *b_lm)

    llm_data = hdr + token_emb_bytes + pos_emb_bytes + blk0_bytes + blk1_bytes + w_lm_bytes + b_lm_bytes
    pad = (512 - (len(llm_data) % 512)) % 512
    llm_data += b'\x00' * pad
    with open(llm_path, 'wb') as f:
        f.write(llm_data)
    print('Generated', llm_path)

