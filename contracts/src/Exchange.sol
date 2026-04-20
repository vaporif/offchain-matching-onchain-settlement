// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";

contract Exchange {
    struct Order {
        uint8 side;      // 0 = Buy, 1 = Sell
        address maker;
        address baseToken;
        address quoteToken;
        uint256 price;
        uint256 quantity;
        uint256 nonce;
        uint256 expiry;
    }

    bytes32 public constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    bytes32 public constant ORDER_TYPEHASH =
        keccak256("Order(uint8 side,address maker,address baseToken,address quoteToken,uint256 price,uint256 quantity,uint256 nonce,uint256 expiry)");
    bytes32 public immutable DOMAIN_SEPARATOR;

    mapping(address => mapping(address => uint256)) public balances;
    mapping(bytes32 => bool) public usedNonces;
    address public operator;

    event Deposited(address indexed user, address indexed token, uint256 amount);
    event Withdrawn(address indexed user, address indexed token, uint256 amount);
    event BatchSettled(uint256 tradeCount);

    error Unauthorized();
    error InsufficientBalance();
    error InvalidSignature();
    error NonceAlreadyUsed();
    error OrderExpired();
    error PriceOutOfRange();

    modifier onlyOperator() {
        if (msg.sender != operator) revert Unauthorized();
        _;
    }

    constructor(address _operator, address _baseToken, address _quoteToken) {
        operator = _operator;
        DOMAIN_SEPARATOR = keccak256(
            abi.encode(
                DOMAIN_TYPEHASH,
                keccak256("HybridExchange"),
                keccak256("1"),
                block.chainid,
                address(this)
            )
        );
    }

    function deposit(address token, uint256 amount) external {
        IERC20(token).transferFrom(msg.sender, address(this), amount);
        balances[msg.sender][token] += amount;
        emit Deposited(msg.sender, token, amount);
    }

    function withdraw(address token, uint256 amount) external {
        if (balances[msg.sender][token] < amount) revert InsufficientBalance();
        balances[msg.sender][token] -= amount;
        IERC20(token).transfer(msg.sender, amount);
        emit Withdrawn(msg.sender, token, amount);
    }

    function settleBatch(
        Order[] calldata makerOrders,
        Order[] calldata takerOrders,
        bytes[] calldata makerSigs,
        bytes[] calldata takerSigs,
        uint256[] calldata quantities,
        uint256[] calldata prices
    ) external onlyOperator {
        uint256 len = makerOrders.length;
        require(
            takerOrders.length == len &&
            makerSigs.length == len &&
            takerSigs.length == len &&
            quantities.length == len &&
            prices.length == len,
            "length mismatch"
        );

        for (uint256 i = 0; i < len; i++) {
            _settleTrade(
                makerOrders[i], takerOrders[i],
                makerSigs[i], takerSigs[i],
                quantities[i], prices[i]
            );
        }

        emit BatchSettled(len);
    }

    function _settleTrade(
        Order calldata maker,
        Order calldata taker,
        bytes calldata makerSig,
        bytes calldata takerSig,
        uint256 quantity,
        uint256 price
    ) internal {
        _verifySignature(maker, makerSig);
        _verifySignature(taker, takerSig);

        if (block.timestamp > maker.expiry) revert OrderExpired();
        if (block.timestamp > taker.expiry) revert OrderExpired();

        bytes32 makerNonceKey = keccak256(abi.encodePacked(maker.maker, maker.nonce));
        bytes32 takerNonceKey = keccak256(abi.encodePacked(taker.maker, taker.nonce));
        if (usedNonces[makerNonceKey]) revert NonceAlreadyUsed();
        if (usedNonces[takerNonceKey]) revert NonceAlreadyUsed();
        usedNonces[makerNonceKey] = true;
        usedNonces[takerNonceKey] = true;

        if (maker.side == 1) {
            if (price < maker.price) revert PriceOutOfRange();
        } else {
            if (price > maker.price) revert PriceOutOfRange();
        }
        if (taker.side == 1) {
            if (price < taker.price) revert PriceOutOfRange();
        } else {
            if (price > taker.price) revert PriceOutOfRange();
        }

        address buyer;
        address seller;
        if (maker.side == 0) {
            buyer = maker.maker;
            seller = taker.maker;
        } else {
            buyer = taker.maker;
            seller = maker.maker;
        }

        uint256 quoteAmount = (quantity * price) / 1e18;
        address baseToken = maker.baseToken;
        address quoteToken = maker.quoteToken;

        if (balances[seller][baseToken] < quantity) revert InsufficientBalance();
        if (balances[buyer][quoteToken] < quoteAmount) revert InsufficientBalance();

        balances[seller][baseToken] -= quantity;
        balances[seller][quoteToken] += quoteAmount;
        balances[buyer][quoteToken] -= quoteAmount;
        balances[buyer][baseToken] += quantity;
    }

    function _verifySignature(Order calldata order, bytes calldata sig) internal view {
        bytes32 structHash = keccak256(
            abi.encode(
                ORDER_TYPEHASH,
                order.side,
                order.maker,
                order.baseToken,
                order.quoteToken,
                order.price,
                order.quantity,
                order.nonce,
                order.expiry
            )
        );
        bytes32 digest = keccak256(
            abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR, structHash)
        );
        address recovered = ECDSA.recover(digest, sig);
        if (recovered != order.maker) revert InvalidSignature();
    }

    function orderHash(Order calldata order) external pure returns (bytes32) {
        return keccak256(
            abi.encode(
                ORDER_TYPEHASH,
                order.side,
                order.maker,
                order.baseToken,
                order.quoteToken,
                order.price,
                order.quantity,
                order.nonce,
                order.expiry
            )
        );
    }
}
