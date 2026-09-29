mod tables {
    include!("cas_des_tables.rs");
}

use tables::{FP, P, PC2, SBOX};

pub fn encrypt(plain: &str) -> String {
    let keys: [[u8; 64]; 3] = [b'1', b'2', b'3'].map(|digit| {
        let mut bytes = [0_u8; 8];
        bytes[1] = digit;
        bytes_to_bits(bytes)
    });
    let units: Vec<u16> = plain.encode_utf16().collect();
    let mut result = String::new();
    for chunk in units.chunks(4) {
        let mut bytes = [0_u8; 8];
        for (index, unit) in chunk.iter().enumerate() {
            bytes[index * 2..index * 2 + 2].copy_from_slice(&unit.to_be_bytes());
        }
        let mut bits = bytes_to_bits(bytes);
        for key in &keys {
            bits = encrypt_block(bits, *key);
        }
        result.push_str(&hex::encode_upper(bits_to_bytes(bits)));
    }
    result
}

fn bytes_to_bits(bytes: [u8; 8]) -> [u8; 64] {
    let mut bits = [0_u8; 64];
    for (index, byte) in bytes.iter().enumerate() {
        for offset in 0..8 {
            bits[index * 8 + offset] = (byte >> (7 - offset)) & 1;
        }
    }
    bits
}

fn bits_to_bytes(bits: [u8; 64]) -> [u8; 8] {
    let mut bytes = [0_u8; 8];
    for (index, bit) in bits.iter().enumerate() {
        bytes[index / 8] |= bit << (7 - index % 8);
    }
    bytes
}

fn encrypt_block(data: [u8; 64], key: [u8; 64]) -> [u8; 64] {
    let mut ip = [0_u8; 64];
    for i in 0..4 {
        for j in 0..8 {
            ip[i * 8 + j] = data[(7 - j) * 8 + 1 + i * 2];
            ip[i * 8 + j + 32] = data[(7 - j) * 8 + i * 2];
        }
    }

    let mut key_bits = [0_u8; 56];
    for i in 0..7 {
        for j in 0..8 {
            key_bits[i * 8 + j] = key[(7 - j) * 8 + i];
        }
    }
    let shifts = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];
    let mut left: [u8; 32] = ip[..32].try_into().unwrap();
    let mut right: [u8; 32] = ip[32..].try_into().unwrap();

    for shift in shifts {
        key_bits[..28].rotate_left(shift);
        key_bits[28..].rotate_left(shift);
        let mut expanded = [0_u8; 48];
        for group in 0..8 {
            for offset in 0..6 {
                let right_index = (group * 4 + offset + 31) % 32;
                expanded[group * 6 + offset] = right[right_index];
            }
        }
        for (index, bit) in expanded.iter_mut().enumerate() {
            *bit ^= key_bits[PC2[index]];
        }

        let mut sbox = [0_u8; 32];
        for group in 0..8 {
            let bits = &expanded[group * 6..group * 6 + 6];
            let row = (bits[0] * 2 + bits[5]) as usize;
            let column = (bits[1] * 8 + bits[2] * 4 + bits[3] * 2 + bits[4]) as usize;
            let value = SBOX[group][row][column];
            for bit in 0..4 {
                sbox[group * 4 + bit] = (value >> (3 - bit)) & 1;
            }
        }
        let mut next_right = [0_u8; 32];
        for index in 0..32 {
            next_right[index] = left[index] ^ sbox[P[index]];
        }
        left = right;
        right = next_right;
    }

    let mut preoutput = [0_u8; 64];
    preoutput[..32].copy_from_slice(&right);
    preoutput[32..].copy_from_slice(&left);
    let mut output = [0_u8; 64];
    for index in 0..64 {
        output[index] = preoutput[FP[index]];
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_cas_javascript() {
        assert_eq!(encrypt(""), "");
        assert_eq!(encrypt("abc"), "39644174795FB4D0");
        assert_eq!(encrypt("测试123"), "7DFBF89ACA3584518BC2282D6215AFE3");
        assert_eq!(encrypt("abcd").len(), 16);
        assert_eq!(encrypt("abcde").len(), 32);
    }
}
