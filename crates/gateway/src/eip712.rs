use alloy::{
    primitives::{Address, B256, U256, keccak256},
    sol,
    sol_types::{SolStruct, SolValue},
};
use types::SignedOrder;

sol! {
    #[derive(Debug)]
    struct Order {
        uint8 side;
        address maker;
        address baseToken;
        address quoteToken;
        uint256 price;
        uint256 quantity;
        uint256 nonce;
        uint256 expiry;
    }
}

sol! {
    #[derive(Debug)]
    struct AuthMessage {
        bytes32 nonce;
        uint256 timestamp;
    }
}

pub fn verify_auth(
    nonce: B256,
    timestamp: u64,
    signature: &[u8],
    domain_separator: B256,
) -> eyre::Result<Address> {
    let auth = AuthMessage {
        nonce,
        timestamp: U256::from(timestamp),
    };
    let struct_hash = auth.eip712_hash_struct();
    let digest = eip712_digest(domain_separator, struct_hash);
    let sig = alloy::signers::Signature::try_from(signature)?;
    let recovered = sig.recover_address_from_prehash(&digest)?;
    Ok(recovered)
}

pub fn recover_signer(order: &SignedOrder, domain_separator: B256) -> eyre::Result<Address> {
    let sol_order = to_sol_order(order);

    let struct_hash = sol_order.eip712_hash_struct();
    let digest = eip712_digest(domain_separator, struct_hash);

    let sig = alloy::signers::Signature::try_from(order.signature.as_ref())?;
    let recovered = sig.recover_address_from_prehash(&digest)?;

    Ok(recovered)
}

pub fn eip712_digest(domain_separator: B256, struct_hash: B256) -> B256 {
    keccak256(
        [
            &[0x19, 0x01],
            domain_separator.as_slice(),
            struct_hash.as_slice(),
        ]
        .concat(),
    )
}

pub fn compute_domain_separator(chain_id: u64, contract_address: Address) -> B256 {
    let domain_type_hash = keccak256(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let name_hash = keccak256("HybridExchange");
    let version_hash = keccak256("1");

    keccak256(
        (
            domain_type_hash,
            name_hash,
            version_hash,
            U256::from(chain_id),
            contract_address,
        )
            .abi_encode(),
    )
}

pub fn order_hash(order: &SignedOrder) -> B256 {
    let sol_order = to_sol_order(order);
    sol_order.eip712_hash_struct()
}

pub fn to_sol_order(order: &SignedOrder) -> Order {
    Order {
        side: match order.side {
            types::Side::Buy => 0,
            types::Side::Sell => 1,
        },
        maker: order.maker,
        baseToken: order.base_token,
        quoteToken: order.quote_token,
        price: order.price,
        quantity: order.quantity,
        nonce: order.nonce,
        expiry: order.expiry,
    }
}

#[must_use]
pub fn cancel_order_hash(nonce: U256) -> B256 {
    let typehash = keccak256(b"CancelOrder(uint256 nonce)");
    keccak256([typehash.as_slice(), &nonce.to_be_bytes::<32>()].concat())
}

pub fn recover_cancel_signer(
    nonce: U256,
    signature: &alloy::primitives::Bytes,
    domain_separator: B256,
) -> eyre::Result<Address> {
    let struct_hash = cancel_order_hash(nonce);
    let digest = eip712_digest(domain_separator, struct_hash);

    let sig = alloy::signers::Signature::try_from(signature.as_ref())?;
    let recovered = sig.recover_address_from_prehash(&digest)?;
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::{
        primitives::{Bytes, U256},
        signers::{Signer, local::PrivateKeySigner},
    };

    #[tokio::test]
    async fn recover_signer_roundtrip() {
        let signer = PrivateKeySigner::random();
        let address = signer.address();
        let contract_addr = Address::with_last_byte(99);
        let domain_separator = compute_domain_separator(31337, contract_addr);

        let order = SignedOrder {
            side: types::Side::Buy,
            maker: address,
            base_token: Address::with_last_byte(1),
            quote_token: Address::with_last_byte(2),
            price: U256::from(1000),
            quantity: U256::from(5),
            nonce: U256::from(1),
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };

        let sol_order = to_sol_order(&order);
        let struct_hash = sol_order.eip712_hash_struct();
        let digest = eip712_digest(domain_separator, struct_hash);
        let sig = signer.sign_hash(&digest).await.unwrap();
        let sig_bytes: Bytes = sig.as_bytes().to_vec().into();

        let signed = SignedOrder {
            signature: sig_bytes,
            ..order
        };

        let recovered = recover_signer(&signed, domain_separator).unwrap();
        assert_eq!(recovered, address);
    }

    #[tokio::test]
    async fn verify_auth_roundtrip() {
        let signer = PrivateKeySigner::random();
        let address = signer.address();
        let domain_separator = compute_domain_separator(31337, Address::with_last_byte(99));

        let nonce = B256::from(rand::random::<[u8; 32]>());
        let timestamp = 1_700_000_000_u64;

        let auth = AuthMessage {
            nonce,
            timestamp: U256::from(timestamp),
        };
        let struct_hash = auth.eip712_hash_struct();
        let digest = eip712_digest(domain_separator, struct_hash);
        let sig = signer.sign_hash(&digest).await.unwrap();

        let recovered = verify_auth(nonce, timestamp, &sig.as_bytes(), domain_separator).unwrap();
        assert_eq!(recovered, address);
    }

    #[tokio::test]
    async fn cancel_order_roundtrip() {
        let signer: PrivateKeySigner = PrivateKeySigner::random();
        let chain_id = 31337u64;
        let contract = Address::with_last_byte(99);
        let domain_separator = compute_domain_separator(chain_id, contract);

        let nonce = U256::from(42);
        let hash = cancel_order_hash(nonce);
        let digest = alloy::primitives::keccak256(
            [&[0x19, 0x01], domain_separator.as_slice(), hash.as_slice()].concat(),
        );
        let sig = signer.sign_hash(&digest).await.unwrap();

        let recovered =
            recover_cancel_signer(nonce, &sig.as_bytes().to_vec().into(), domain_separator)
                .unwrap();
        assert_eq!(recovered, signer.address());
    }
}
