const WYP_0: u64 = 0xa0761d6478bd642f;
const WYP_1: u64 = 0xe7037ed1a0b428db;
const WYP_2: u64 = 0x8ebc6af09c88c6e3;
const WYP_3: u64 = 0x589965cc75374cc3;
const WYP_4: u64 = 0x1d8e4e27c47d124f;

#[inline(always)]
fn wymum(lhs: u64, rhs: u64) -> u64 {
    let product = (lhs as u128).wrapping_mul(rhs as u128);
    let low = product as u64;
    let high = (product >> 64) as u64;
    low ^ high
}

#[inline(always)]
fn u32(input: &[u8], offset: usize) -> u64 {
    let bytes = &input[offset..offset + 4];
    u32::from_le_bytes(bytes.try_into().unwrap()) as u64
}

#[inline(always)]
fn i64_le(input: &[u8], offset: usize) -> u64 {
    let bytes = &input[offset..offset + 8];
    u64::from_le_bytes(bytes.try_into().unwrap())
}

#[inline(always)]
fn u64_rotate32(input: &[u8], offset: usize) -> u64 {
    (u32(input, offset) << 32) | u32(input, offset + 4)
}

#[inline(always)]
fn wyr3(input: &[u8], index: usize, k: usize) -> u64 {
    ((input[index] as u64) << 16)
        | ((input[index + (k >> 1)] as u64) << 8)
        | (input[index + k - 1] as u64)
}

fn wy_hash64(input: &[u8], off: usize, len: usize) -> u64 {
    if len == 0 {
        return 0;
    }

    let mut seed = 0u64;
    let mut see1 = 0u64;
    let mut p = off;
    let mut i = len;

    if len < 4 {
        return wymum(
            wymum(wyr3(input, p, len) ^ WYP_0, WYP_1),
            (len as u64) ^ WYP_4,
        );
    } else if len <= 8 {
        return wymum(
            wymum(u32(input, p) ^ WYP_0, u32(input, p + len - 4) ^ WYP_1),
            (len as u64) ^ WYP_4,
        );
    } else if len <= 16 {
        return wymum(
            wymum(
                u64_rotate32(input, p) ^ WYP_0,
                u64_rotate32(input, p + len - 8) ^ WYP_1,
            ),
            (len as u64) ^ WYP_4,
        );
    }

    while i > 32 {
        seed = wymum(
            i64_le(input, p) ^ seed ^ WYP_0,
            i64_le(input, p + 8) ^ seed ^ WYP_1,
        );
        see1 = wymum(
            i64_le(input, p + 16) ^ see1 ^ WYP_2,
            i64_le(input, p + 24) ^ see1 ^ WYP_3,
        );
        p += 32;
        i -= 32;
    }

    if i < 4 {
        seed = wymum(wyr3(input, p, i) ^ seed ^ WYP_0, seed ^ WYP_1);
    } else if i <= 8 {
        seed = wymum(
            u32(input, p) ^ seed ^ WYP_0,
            u32(input, p + i - 4) ^ seed ^ WYP_1,
        );
    } else if i <= 16 {
        seed = wymum(
            u64_rotate32(input, p) ^ seed ^ WYP_0,
            u64_rotate32(input, p + i - 8) ^ seed ^ WYP_1,
        );
    } else {
        seed = wymum(
            u64_rotate32(input, p) ^ seed ^ WYP_0,
            u64_rotate32(input, p + 8) ^ seed ^ WYP_1,
        );
        see1 = wymum(
            u64_rotate32(input, p + i - 8) ^ see1 ^ WYP_2,
            see1 ^ WYP_3,
        );
    }

    wymum(seed ^ see1, (len as u64) ^ WYP_4)
}

#[derive(Default)]
pub struct WyHash;

impl WyHash {
    pub fn hash(input: &[u8], off: usize, len: usize) -> u64 {
        wy_hash64(input, off, len)
    }

    pub fn hash_u64(input: u64) -> u64 {
        let hi = input & 0xFFFF_FFFF;
        let lo = input >> 32;
        wymum(wymum(hi ^ WYP_0, lo ^ WYP_1), 8 ^ WYP_4)
    }

    pub fn hash_u32(input: u32) -> u64 {
        let v = input as u64;
        wymum(wymum(v ^ WYP_0, v ^ WYP_1), 4 ^ WYP_4)
    }

    pub fn hash_u16(input: u16) -> u64 {
        let hi = (input >> 8) as u64;
        let wyr3 = hi | (hi << 8) | ((input & 0xFF) as u64) << 16;
        wymum(wymum(wyr3 ^ WYP_0, WYP_1), 2 ^ WYP_4)
    }

    pub fn hash_u8(input: u8) -> u64 {
        let hi = input as u64;
        let wyr3 = hi | (hi << 8) | (hi << 16);
        wymum(wymum(wyr3 ^ WYP_0, WYP_1), 1 ^ WYP_4)
    }
}