use alloy::primitives::{Address, B256, U256};

#[must_use]
pub const fn u256_to_bytes(v: U256) -> [u8; 32] {
    v.to_be_bytes::<32>()
}

#[must_use]
pub const fn u256_from_bytes(b: &[u8]) -> U256 {
    U256::from_be_slice(b)
}

#[must_use]
pub fn address_to_bytes(a: Address) -> [u8; 20] {
    *a.0
}

#[must_use]
pub fn address_from_bytes(b: &[u8]) -> Address {
    let mut buf = [0u8; 20];
    buf.copy_from_slice(b);
    Address::from(buf)
}

#[must_use]
pub const fn b256_to_bytes(v: B256) -> [u8; 32] {
    v.0
}

#[must_use]
pub fn b256_from_bytes(b: &[u8]) -> B256 {
    let mut buf = [0u8; 32];
    buf.copy_from_slice(b);
    B256::from(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u256_roundtrip() {
        let values = [
            U256::ZERO,
            U256::from(1u64),
            U256::from(u64::MAX),
            U256::MAX,
        ];
        for v in values {
            assert_eq!(u256_from_bytes(&u256_to_bytes(v)), v);
        }
    }

    #[test]
    fn address_roundtrip() {
        let addr = Address::from([0xAB; 20]);
        assert_eq!(address_from_bytes(&address_to_bytes(addr)), addr);

        let zero = Address::ZERO;
        assert_eq!(address_from_bytes(&address_to_bytes(zero)), zero);
    }

    #[test]
    fn b256_roundtrip() {
        let val = B256::from([0xCD; 32]);
        assert_eq!(b256_from_bytes(&b256_to_bytes(val)), val);

        let zero = B256::ZERO;
        assert_eq!(b256_from_bytes(&b256_to_bytes(zero)), zero);
    }
}
