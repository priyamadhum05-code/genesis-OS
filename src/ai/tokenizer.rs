//! Project Genesis — Standard Llama-2 BPE Tokenizer (Stage 14).
//!
//! Implements a zero-copy / minimal-allocation BPE tokenizer compatible with `llama2.c`:
//! - Parses standard `tokenizer.bin` binaries: `(max_token_length: i32, [(score: f32, len: i32, bytes)] * vocab)`
//! - Byte-pair encoding (BPE) with greedy score maximization
//! - Token decoding with `<0xNN>` hex byte fallback and BOS handling

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct Tokenizer {
    pub max_token_length: usize,
    pub vocab: Vec<String>,
    pub scores: Vec<f32>,
    pub bos_id: usize,
    pub eos_id: usize,
}

impl Tokenizer {
    /// Parses a standard `llama2.c` `tokenizer.bin` image into a `Tokenizer`.
    pub fn parse(data: &[u8], vocab_size: usize) -> Result<Self, &'static str> {
        if data.len() < 4 {
            return Err("Tokenizer data too short for header");
        }

        let max_len = i32::from_le_bytes([data[0], data[1], data[2], data[3]]).max(1) as usize;
        let mut offset = 4;
        let mut vocab = Vec::with_capacity(vocab_size);
        let mut scores = Vec::with_capacity(vocab_size);

        for _ in 0..vocab_size {
            if offset + 8 > data.len() {
                break;
            }
            let score = f32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]);
            offset += 4;
            let len = i32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]) as usize;
            offset += 4;

            if offset + len > data.len() {
                return Err("Unexpected EOF reading tokenizer vocabulary entry");
            }

            let piece_bytes = &data[offset..offset + len];
            let piece = String::from_utf8_lossy(piece_bytes).into_owned();
            offset += len;

            vocab.push(piece);
            scores.push(score);
        }

        Ok(Self {
            max_token_length: max_len,
            vocab,
            scores,
            bos_id: 1,
            eos_id: 2,
        })
    }

    pub fn default_fallback(vocab_size: usize) -> Self {
        let mut vocab = Vec::with_capacity(vocab_size);
        let mut scores = Vec::with_capacity(vocab_size);
        vocab.push(String::from("<unk>"));
        scores.push(0.0);
        vocab.push(String::from("<s>"));
        scores.push(0.0);
        vocab.push(String::from("</s>"));
        scores.push(0.0);

        for i in 3..vocab_size {
            if i < 256 + 3 {
                let b = (i - 3) as u8;
                if b.is_ascii_graphic() || b == b' ' {
                    vocab.push(String::from(b as char));
                } else {
                    vocab.push(format_byte_hex(b));
                }
            } else {
                let mut s = String::with_capacity(8);
                s.push_str("t_");
                let mut buf = [0u8; 16];
                let mut val = i;
                let mut p = 0;
                if val == 0 { buf[0] = b'0'; p = 1; }
                else {
                    while val > 0 {
                        buf[p] = b'0' + (val % 10) as u8;
                        val /= 10;
                        p += 1;
                    }
                    buf[..p].reverse();
                }
                if let Ok(st) = core::str::from_utf8(&buf[..p]) {
                    s.push_str(st);
                }
                vocab.push(s);
            }
            scores.push(0.0);
        }

        Self {
            max_token_length: 8,
            vocab,
            scores,
            bos_id: 1,
            eos_id: 2,
        }
    }

    /// Finds the vocabulary token ID matching `piece`.
    pub fn find_token(&self, piece: &str) -> Option<usize> {
        for (i, v) in self.vocab.iter().enumerate() {
            if v == piece {
                return Some(i);
            }
        }
        None
    }

    /// Encodes a text prompt into token IDs via BPE merging.
    pub fn encode(&self, text: &str, add_bos: bool) -> Vec<usize> {
        let mut tokens = Vec::new();
        if add_bos && self.vocab.len() > self.bos_id {
            tokens.push(self.bos_id);
        }
        if text.is_empty() {
            return tokens;
        }

        // 1. Initial character/byte tokens
        for b in text.bytes() {
            let single = [b];
            let s = core::str::from_utf8(&single).unwrap_or("");
            if let Some(id) = self.find_token(s) {
                tokens.push(id);
            } else {
                // Byte fallback: <0xNN>
                let hex_str = format_byte_hex(b);
                if let Some(id) = self.find_token(&hex_str) {
                    tokens.push(id);
                } else if (b as usize) < self.vocab.len() {
                    tokens.push(b as usize);
                }
            }
        }

        // 2. Greedy BPE merge loop
        loop {
            let mut best_score = -1e10f32;
            let mut best_idx = None;
            let mut best_id = 0;

            let mut i = 0;
            while i + 1 < tokens.len() {
                let tok1 = tokens[i];
                let tok2 = tokens[i + 1];
                if tok1 < self.vocab.len() && tok2 < self.vocab.len() {
                    let mut merged = String::with_capacity(self.vocab[tok1].len() + self.vocab[tok2].len());
                    merged.push_str(&self.vocab[tok1]);
                    merged.push_str(&self.vocab[tok2]);
                    if let Some(id) = self.find_token(&merged) {
                        let score = self.scores[id];
                        if score > best_score {
                            best_score = score;
                            best_idx = Some(i);
                            best_id = id;
                        }
                    }
                }
                i += 1;
            }

            if let Some(idx) = best_idx {
                tokens[idx] = best_id;
                tokens.remove(idx + 1);
            } else {
                break;
            }
        }

        tokens
    }

    /// Decodes a token into UTF-8 bytes, translating `<0xNN>` byte fallbacks.
    pub fn decode<'a>(&'a self, _prev_token: usize, token: usize, scratch: &'a mut [u8; 8]) -> &'a [u8] {
        if token >= self.vocab.len() {
            return b"";
        }

        let piece = &self.vocab[token];

        // Check if piece is a raw byte fallback of the form "<0xNN>"
        if piece.len() == 6 && piece.starts_with("<0x") && piece.ends_with('>') {
            let hex_part = &piece[3..5];
            if let Ok(byte_val) = u8::from_str_radix(hex_part, 16) {
                scratch[0] = byte_val;
                return &scratch[..1];
            }
        }

        piece.as_bytes()
    }
}

pub static mut GLOBAL_TOKENIZER: Option<Tokenizer> = None;

pub fn set_global_tokenizer(tok: Tokenizer) {
    unsafe {
        GLOBAL_TOKENIZER = Some(tok);
    }
}

#[allow(static_mut_refs)]
pub fn get_global_tokenizer() -> Option<&'static Tokenizer> {
    unsafe {
        GLOBAL_TOKENIZER.as_ref()
    }
}

fn format_byte_hex(b: u8) -> String {
    let mut s = String::with_capacity(6);
    s.push_str("<0x");
    let hex_chars = b"0123456789ABCDEF";
    s.push(hex_chars[(b >> 4) as usize] as char);
    s.push(hex_chars[(b & 0x0F) as usize] as char);
    s.push('>');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn tokenizer_parse_and_bpe_encode_decode() {
        // Construct a small mock tokenizer.bin with 10 tokens:
        // 0: <unk>, 1: <s>, 2: </s>
        // 3: 'a', 4: 'b', 5: 'c'
        // 6: 'ab' (score 10.0)
        // 7: 'bc' (score 5.0)
        // 8: 'abc' (score 20.0)
        // 9: <0x0A> (newline fallback)
        let mut bin = Vec::new();
        bin.extend_from_slice(&4i32.to_le_bytes()); // max_token_length = 4

        let tokens_data = [
            (0.0f32, "<unk>"),
            (0.0, "<s>"),
            (0.0, "</s>"),
            (0.0, "a"),
            (0.0, "b"),
            (0.0, "c"),
            (10.0, "ab"),
            (5.0, "bc"),
            (20.0, "abc"),
            (0.0, "<0x0A>"),
        ];

        for (score, piece) in tokens_data {
            bin.extend_from_slice(&score.to_le_bytes());
            bin.extend_from_slice(&(piece.len() as i32).to_le_bytes());
            bin.extend_from_slice(piece.as_bytes());
        }

        let tok = Tokenizer::parse(&bin, tokens_data.len()).expect("failed to parse tokenizer");
        assert_eq!(tok.max_token_length, 4);
        assert_eq!(tok.vocab.len(), 10);

        // Test BPE encoding: "abc" should merge to single token 8 ("abc")
        let encoded = tok.encode("abc", true);
        assert_eq!(encoded, vec![1, 8]); // [<s>, abc]

        // Test BPE encoding: "ab" should merge to token 6 ("ab")
        let encoded_ab = tok.encode("ab", false);
        assert_eq!(encoded_ab, vec![6]);

        // Test decoding: token 8 -> "abc"
        let mut scratch = [0u8; 8];
        let dec = tok.decode(1, 8, &mut scratch);
        assert_eq!(dec, b"abc");

        // Test byte fallback decode: token 9 (<0x0A>) -> \n (0x0A)
        let dec_nl = tok.decode(8, 9, &mut scratch);
        assert_eq!(dec_nl, b"\n");
    }

    #[test]
    fn tokenizer_empty_and_unknown_handling() {
        let mut bin = Vec::new();
        bin.extend_from_slice(&1i32.to_le_bytes());
        let tokens_data = [(0.0f32, "<unk>"), (0.0, "<s>"), (0.0, "</s>")];
        for (score, piece) in tokens_data {
            bin.extend_from_slice(&score.to_le_bytes());
            bin.extend_from_slice(&(piece.len() as i32).to_le_bytes());
            bin.extend_from_slice(piece.as_bytes());
        }
        let tok = Tokenizer::parse(&bin, 3).expect("parse");

        let empty = tok.encode("", false);
        assert!(empty.is_empty());

        let empty_with_bos = tok.encode("", true);
        assert_eq!(empty_with_bos, vec![1]);

        let mut scratch = [0u8; 8];
        let out_of_bounds = tok.decode(0, 999, &mut scratch);
        assert_eq!(out_of_bounds, b"");
    }

    #[test]
    fn tokenizer_default_fallback_test() {
        let tok = Tokenizer::default_fallback(64);
        assert_eq!(tok.vocab.len(), 64);
        assert_eq!(tok.bos_id, 1);
        assert_eq!(tok.vocab[1], "<s>");
        assert_eq!(tok.vocab[2], "</s>");
        let enc = tok.encode("a b", true);
        assert!(!enc.is_empty());
        assert_eq!(enc[0], 1); // BOS
    }
}
