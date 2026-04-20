use alloy::sol;

sol! {
    #[sol(rpc)]
    contract Exchange {
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

        function deposit(address token, uint256 amount) external;
        function withdraw(address token, uint256 amount) external;
        function settleBatch(
            Order[] calldata makerOrders,
            Order[] calldata takerOrders,
            bytes[] calldata makerSigs,
            bytes[] calldata takerSigs,
            uint256[] calldata quantities,
            uint256[] calldata prices
        ) external;
        function balances(address user, address token) external view returns (uint256);

        event Deposited(address indexed user, address indexed token, uint256 amount);
        event Withdrawn(address indexed user, address indexed token, uint256 amount);
        event BatchSettled(uint256 tradeCount);
    }
}
