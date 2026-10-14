pragma solidity ^0.8.0;
contract Token is ERC20 {
    uint256 public totalSupply;
    event Transfer(address from, address to);
    error Unauthorized();
    modifier onlyOwner() { _; }
    constructor() {}
    function transfer(address to) public returns (bool) { return true; }
    struct Account { uint balance; }
}
interface IToken { function f() external; }
library Math { function add(uint a) internal pure returns (uint) { return a; } }
