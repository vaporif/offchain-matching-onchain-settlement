// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, console} from "forge-std/Test.sol";
import {Exchange} from "../src/Exchange.sol";
import {MockERC20} from "../src/MockERC20.sol";

contract ExchangeTest is Test {
    Exchange exchange;
    MockERC20 baseToken;
    MockERC20 quoteToken;

    address operator;
    uint256 makerKey;
    address maker;
    uint256 takerKey;
    address taker;

    function setUp() public {
        operator = address(this);
        (maker, makerKey) = makeAddrAndKey("maker");
        (taker, takerKey) = makeAddrAndKey("taker");

        baseToken = new MockERC20("Base", "BASE");
        quoteToken = new MockERC20("Quote", "QUOTE");
        exchange = new Exchange(operator, address(baseToken), address(quoteToken));

        baseToken.mint(maker, 1000e18);
        quoteToken.mint(taker, 1000e18);

        vm.prank(maker);
        baseToken.approve(address(exchange), type(uint256).max);
        vm.prank(taker);
        quoteToken.approve(address(exchange), type(uint256).max);

        vm.prank(maker);
        exchange.deposit(address(baseToken), 1000e18);
        vm.prank(taker);
        exchange.deposit(address(quoteToken), 1000e18);
    }

    function _signOrder(Exchange.Order memory order, uint256 privateKey) internal view returns (bytes memory) {
        bytes32 structHash = keccak256(
            abi.encode(
                exchange.ORDER_TYPEHASH(),
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
            abi.encodePacked("\x19\x01", exchange.DOMAIN_SEPARATOR(), structHash)
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(privateKey, digest);
        return abi.encodePacked(r, s, v);
    }

    function _makerSellOrder(uint256 price, uint256 qty, uint256 nonce) internal view returns (Exchange.Order memory) {
        return Exchange.Order({
            side: 1,
            maker: maker,
            baseToken: address(baseToken),
            quoteToken: address(quoteToken),
            price: price,
            quantity: qty,
            nonce: nonce,
            expiry: block.timestamp + 1 hours
        });
    }

    function _takerBuyOrder(uint256 price, uint256 qty, uint256 nonce) internal view returns (Exchange.Order memory) {
        return Exchange.Order({
            side: 0,
            maker: taker,
            baseToken: address(baseToken),
            quoteToken: address(quoteToken),
            price: price,
            quantity: qty,
            nonce: nonce,
            expiry: block.timestamp + 1 hours
        });
    }

    function test_deposit_creditsBalance() public view {
        assertEq(exchange.balances(maker, address(baseToken)), 1000e18);
        assertEq(exchange.balances(taker, address(quoteToken)), 1000e18);
    }

    function test_withdraw_exceedsBalance_reverts() public {
        vm.prank(maker);
        vm.expectRevert(Exchange.InsufficientBalance.selector);
        exchange.withdraw(address(baseToken), 2000e18);
    }

    function test_withdraw_happyPath() public {
        vm.prank(maker);
        exchange.withdraw(address(baseToken), 500e18);
        assertEq(exchange.balances(maker, address(baseToken)), 500e18);
        assertEq(baseToken.balanceOf(maker), 500e18);
    }

    function test_settleBatch_happyPath_updatesBalances() public {
        Exchange.Order memory makerOrder = _makerSellOrder(1e18, 10e18, 1);
        Exchange.Order memory takerOrder = _takerBuyOrder(1e18, 10e18, 1);

        bytes memory makerSig = _signOrder(makerOrder, makerKey);
        bytes memory takerSig = _signOrder(takerOrder, takerKey);

        Exchange.Order[] memory makerOrders = new Exchange.Order[](1);
        Exchange.Order[] memory takerOrders = new Exchange.Order[](1);
        bytes[] memory makerSigs = new bytes[](1);
        bytes[] memory takerSigs = new bytes[](1);
        uint256[] memory quantities = new uint256[](1);
        uint256[] memory prices = new uint256[](1);

        makerOrders[0] = makerOrder;
        takerOrders[0] = takerOrder;
        makerSigs[0] = makerSig;
        takerSigs[0] = takerSig;
        quantities[0] = 10e18;
        prices[0] = 1e18;

        exchange.settleBatch(makerOrders, takerOrders, makerSigs, takerSigs, quantities, prices);

        assertEq(exchange.balances(maker, address(baseToken)), 990e18);
        assertEq(exchange.balances(maker, address(quoteToken)), 10e18);
        assertEq(exchange.balances(taker, address(baseToken)), 10e18);
        assertEq(exchange.balances(taker, address(quoteToken)), 990e18);
    }

    function test_settleBatch_forgedSignature_reverts() public {
        Exchange.Order memory makerOrder = _makerSellOrder(1e18, 10e18, 1);
        Exchange.Order memory takerOrder = _takerBuyOrder(1e18, 10e18, 1);

        bytes memory makerSig = _signOrder(makerOrder, takerKey); // WRONG KEY
        bytes memory takerSig = _signOrder(takerOrder, takerKey);

        Exchange.Order[] memory makerOrders = new Exchange.Order[](1);
        Exchange.Order[] memory takerOrders = new Exchange.Order[](1);
        bytes[] memory makerSigs = new bytes[](1);
        bytes[] memory takerSigs = new bytes[](1);
        uint256[] memory quantities = new uint256[](1);
        uint256[] memory prices = new uint256[](1);

        makerOrders[0] = makerOrder;
        takerOrders[0] = takerOrder;
        makerSigs[0] = makerSig;
        takerSigs[0] = takerSig;
        quantities[0] = 10e18;
        prices[0] = 1e18;

        vm.expectRevert(Exchange.InvalidSignature.selector);
        exchange.settleBatch(makerOrders, takerOrders, makerSigs, takerSigs, quantities, prices);
    }

    function test_settleBatch_expiredOrder_reverts() public {
        Exchange.Order memory makerOrder = _makerSellOrder(1e18, 10e18, 1);
        makerOrder.expiry = block.timestamp - 1;
        Exchange.Order memory takerOrder = _takerBuyOrder(1e18, 10e18, 1);

        bytes memory makerSig = _signOrder(makerOrder, makerKey);
        bytes memory takerSig = _signOrder(takerOrder, takerKey);

        Exchange.Order[] memory makerOrders = new Exchange.Order[](1);
        Exchange.Order[] memory takerOrders = new Exchange.Order[](1);
        bytes[] memory makerSigs = new bytes[](1);
        bytes[] memory takerSigs = new bytes[](1);
        uint256[] memory quantities = new uint256[](1);
        uint256[] memory prices = new uint256[](1);

        makerOrders[0] = makerOrder;
        takerOrders[0] = takerOrder;
        makerSigs[0] = makerSig;
        takerSigs[0] = takerSig;
        quantities[0] = 10e18;
        prices[0] = 1e18;

        vm.expectRevert(Exchange.OrderExpired.selector);
        exchange.settleBatch(makerOrders, takerOrders, makerSigs, takerSigs, quantities, prices);
    }

    function test_settleBatch_doubleSpentNonce_reverts() public {
        Exchange.Order memory makerOrder = _makerSellOrder(1e18, 5e18, 1);
        Exchange.Order memory takerOrder = _takerBuyOrder(1e18, 5e18, 1);

        bytes memory makerSig = _signOrder(makerOrder, makerKey);
        bytes memory takerSig = _signOrder(takerOrder, takerKey);

        Exchange.Order[] memory makerOrders = new Exchange.Order[](1);
        Exchange.Order[] memory takerOrders = new Exchange.Order[](1);
        bytes[] memory makerSigs = new bytes[](1);
        bytes[] memory takerSigs = new bytes[](1);
        uint256[] memory quantities = new uint256[](1);
        uint256[] memory prices = new uint256[](1);

        makerOrders[0] = makerOrder;
        takerOrders[0] = takerOrder;
        makerSigs[0] = makerSig;
        takerSigs[0] = takerSig;
        quantities[0] = 5e18;
        prices[0] = 1e18;

        exchange.settleBatch(makerOrders, takerOrders, makerSigs, takerSigs, quantities, prices);

        vm.expectRevert(Exchange.NonceAlreadyUsed.selector);
        exchange.settleBatch(makerOrders, takerOrders, makerSigs, takerSigs, quantities, prices);
    }

    function test_settleBatch_priceOutOfRange_reverts() public {
        Exchange.Order memory makerOrder = _makerSellOrder(2e18, 10e18, 1);
        Exchange.Order memory takerOrder = _takerBuyOrder(1e18, 10e18, 1);

        bytes memory makerSig = _signOrder(makerOrder, makerKey);
        bytes memory takerSig = _signOrder(takerOrder, takerKey);

        Exchange.Order[] memory makerOrders = new Exchange.Order[](1);
        Exchange.Order[] memory takerOrders = new Exchange.Order[](1);
        bytes[] memory makerSigs = new bytes[](1);
        bytes[] memory takerSigs = new bytes[](1);
        uint256[] memory quantities = new uint256[](1);
        uint256[] memory prices = new uint256[](1);

        makerOrders[0] = makerOrder;
        takerOrders[0] = takerOrder;
        makerSigs[0] = makerSig;
        takerSigs[0] = takerSig;
        quantities[0] = 10e18;
        prices[0] = 1e18;

        vm.expectRevert(Exchange.PriceOutOfRange.selector);
        exchange.settleBatch(makerOrders, takerOrders, makerSigs, takerSigs, quantities, prices);
    }

    function test_onlyOperator_reverts() public {
        Exchange.Order[] memory empty = new Exchange.Order[](0);
        bytes[] memory emptySigs = new bytes[](0);
        uint256[] memory emptyUints = new uint256[](0);

        vm.prank(maker);
        vm.expectRevert(Exchange.Unauthorized.selector);
        exchange.settleBatch(empty, empty, emptySigs, emptySigs, emptyUints, emptyUints);
    }

    function test_cancelNonce_burnsNonce() public {
        exchange.cancelNonce(maker, 1);
        bytes32 nonceKey = keccak256(abi.encodePacked(maker, uint256(1)));
        assertTrue(exchange.usedNonces(nonceKey));
    }

    function test_cancelNonce_emitsEvent() public {
        vm.expectEmit(true, true, false, true);
        emit Exchange.NonceCancelled(address(this), maker, 1);
        exchange.cancelNonce(maker, 1);
    }

    function test_cancelNonce_preventsSettlement() public {
        exchange.cancelNonce(maker, 1);

        Exchange.Order memory makerOrder = _makerSellOrder(1e18, 10e18, 1);
        Exchange.Order memory takerOrder = _takerBuyOrder(1e18, 10e18, 1);

        bytes memory makerSig = _signOrder(makerOrder, makerKey);
        bytes memory takerSig = _signOrder(takerOrder, takerKey);

        Exchange.Order[] memory makerOrders = new Exchange.Order[](1);
        Exchange.Order[] memory takerOrders = new Exchange.Order[](1);
        bytes[] memory makerSigs = new bytes[](1);
        bytes[] memory takerSigs = new bytes[](1);
        uint256[] memory quantities = new uint256[](1);
        uint256[] memory prices = new uint256[](1);

        makerOrders[0] = makerOrder;
        takerOrders[0] = takerOrder;
        makerSigs[0] = makerSig;
        takerSigs[0] = takerSig;
        quantities[0] = 10e18;
        prices[0] = 1e18;

        vm.expectRevert(Exchange.NonceAlreadyUsed.selector);
        exchange.settleBatch(makerOrders, takerOrders, makerSigs, takerSigs, quantities, prices);
    }

    function test_cancelNonce_idempotent() public {
        exchange.cancelNonce(maker, 1);
        exchange.cancelNonce(maker, 1);
        bytes32 nonceKey = keccak256(abi.encodePacked(maker, uint256(1)));
        assertTrue(exchange.usedNonces(nonceKey));
    }

    function test_cancelNonce_anyoneCanCall() public {
        vm.prank(taker);
        exchange.cancelNonce(maker, 42);
        bytes32 nonceKey = keccak256(abi.encodePacked(maker, uint256(42)));
        assertTrue(exchange.usedNonces(nonceKey));
    }
}
