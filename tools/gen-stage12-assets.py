#!/usr/bin/env python3
import math
import os
import struct

os.makedirs('build/esp', exist_ok=True)

# 1. TINY_LLAMA.BIN
llama_path = 'build/esp/TINY_LLAMA.BIN'
if not os.path.exists(llama_path):
    dim = 64
    hidden_dim = 128
    n_layers = 2
    n_heads = 4
    n_kv_heads = 4
    vocab_size = 128
    seq_len = 32

    hdr = struct.pack('<7i', dim, hidden_dim, n_layers, n_heads, n_kv_heads, vocab_size, seq_len)

    def make_floats(n, scale=0.01):
        return struct.pack(f'<{n}f', *[math.sin((i + 1) * 0.1) * scale for i in range(n)])

    def make_ones(n):
        return struct.pack(f'<{n}f', *[1.0] * n)

    emb = make_floats(vocab_size * dim, 0.05)
    rms_att = make_ones(n_layers * dim)
    wq = make_floats(n_layers * dim * dim, 0.02)
    wk = make_floats(n_layers * dim * dim, 0.02)
    wv = make_floats(n_layers * dim * dim, 0.02)
    wo = make_floats(n_layers * dim * dim, 0.02)
    rms_ffn = make_ones(n_layers * dim)
    w1 = make_floats(n_layers * dim * hidden_dim, 0.02)
    w2 = make_floats(n_layers * hidden_dim * dim, 0.02)
    w3 = make_floats(n_layers * dim * hidden_dim, 0.02)
    rms_final = make_ones(dim)

    data = hdr + emb + rms_att + wq + wk + wv + wo + rms_ffn + w1 + w2 + w3 + rms_final
    with open(llama_path, 'wb') as f:
        f.write(data)
    print('Generated', llama_path)

# 2. TOKENIZER.BIN
tok_path = 'build/esp/TOKENIZER.BIN'
if not os.path.exists(tok_path):
    tok_data = bytearray()
    tok_data.extend(struct.pack('<I', 1)) # max_token_length
    for i in range(128):
        tok_data.extend(struct.pack('<fi', 0.0, 1))
        tok_data.append(i)

    with open(tok_path, 'wb') as f:
        f.write(tok_data)
    print('Generated', tok_path)
