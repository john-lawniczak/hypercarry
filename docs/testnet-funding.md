# Hyperliquid testnet funding

This guide explains how to fund a Hyperliquid testnet trading account without
mixing the account owner's wallet with an automated API/agent wallet. Faucet
rules can change; the values below were checked against Hyperliquid's published
documentation on 2026-08-30.

## Use two different addresses

Hypercarry's testnet operator expects two identities:

- The **master/trading wallet** owns the Hyperliquid account, receives mock
  collateral, and authorizes agents.
- The **API/agent wallet** signs automated actions on behalf of the master. It
  does not receive the faucet claim and must not be used for account queries.

For example, a deployment might use:

```text
Master/trading wallet: 0xC0ffee0000000000000000000000000000000000
API/agent wallet:      0xA6e1700000000000000000000000000000000000
```

These are fictional, full-length EVM addresses. Do not send funds to them.
Names such as `0xCoffeeBabe` are memorable but are not valid EVM addresses:
addresses contain exactly 40 hexadecimal characters after `0x`, and `o` is not
a hexadecimal character.

Keep both private keys and recovery phrases out of this repository,
configuration files, environment variables, shell history, logs, and issue
trackers. Public addresses and non-secret signer aliases may be configured.

## Obtain mock USDC for HyperCore trading

The official testnet drip grants 1,000 mock USDC once, but the connected master
address must first have made a deposit on Hyperliquid mainnet. No mainnet HYPE
purchase is required.

The simplest documented eligibility path is a mainnet USDC deposit through
Arbitrum:

1. Select the intended **master/trading wallet** in an EVM wallet extension.
   Do not select the API/agent wallet.
2. On Arbitrum, obtain at least 5 native USDC plus enough ETH to pay the
   Arbitrum deposit transaction's gas. Depositing slightly more than the exact
   minimum avoids boundary mistakes.
3. Open the official [Hyperliquid mainnet application][mainnet-app], connect
   the master wallet, enable trading, and use its deposit flow.
4. Deposit native USDC from Arbitrum. Hyperliquid documents 5 USDC as the
   minimum; a smaller deposit is not credited and may be unrecoverable. Do not
   send USDT, ETH, ARB, bridged USDC variants, or another token to the Arbitrum
   USDC deposit route.
5. After the mainnet deposit is credited, open the official [testnet
   drip][testnet-drip] and connect the **same master address**.
6. Claim the one-time 1,000 mock USDC allocation. Testnet assets have no
   mainnet value.
7. On testnet, authorize the separate API/agent address for this master
   account. The authorization is network-specific; independently verify it
   before starting Hypercarry.

The mainnet deposit uses real funds. Confirm the domain, wallet address,
network, token, and amount in the wallet before approving it. Hyperliquid's
[Arbitrum deposit guidance][arbitrum-deposit] and [bridge
documentation][bridge2] are authoritative for the current token and minimum.

## Understand common faucet errors

`Cannot claim drip because user 0x... does not exist on mainnet` means the
address connected to the testnet drip has no credited Hyperliquid mainnet
deposit. It does not mean the address needs to hold mainnet HYPE.

Check, in order:

1. The connected address is the master/trading wallet, not the API/agent
   wallet.
2. The same address was used for both the mainnet deposit and testnet drip.
3. The mainnet deposit has been credited in the Hyperliquid application.
4. An Arbitrum USDC deposit met the published 5 USDC minimum and used native
   USDC.

Do not solve this error by funding the API/agent address or reusing the master
private key in the automated signer.

## Unified versus standard balances

Hyperliquid's [unified account mode][account-modes] uses one USDC balance for
both spot trading and perpetual collateral. In that mode, mock USDC may appear
in the spot state while still being available to perpetual orders; an empty
individual Perps state does not prove the account is unfunded. Standard mode
keeps Spot and Perps balances separate and may require an internal
Spot-to-Perps transfer.

Check the official `userAbstraction` response before selecting the collateral
source. Hypercarry's live testnet gate requires `unifiedAccount` to match
`risk.account_mode: unified_account`, or `disabled` to match `standard`, and
then queries the corresponding balance endpoint. Do not change account mode
merely to make a legacy Perps balance field nonzero.

## Testnet HYPE is a separate asset

Mock USDC funds HyperCore spot/perpetual trading. Testnet HYPE pays gas for
HyperEVM smart-contract transactions. Hypercarry's current operator uses the
HyperCore exchange API, so it needs mock USDC but does not need HyperEVM HYPE
to place or cancel orders.

If separate HyperEVM testing requires gas, Hyperliquid lists third-party
[testnet HYPE faucets][hyperevm-tools]. Their eligibility rules and rate limits
are controlled by the faucet providers and may change. Mainnet HYPE is not a
prerequisite stated by Hyperliquid for these faucets.

## Operator configuration mapping

After funding and agent authorization, map the identities without including
secrets:

```json
{
  "account_address": "0xC0ffee0000000000000000000000000000000000",
  "authorized_signer_address": "0xA6e1700000000000000000000000000000000000",
  "signer_alias": "protected-store/hypercarry-testnet-agent"
}
```

Queries and final open-order checks use `account_address`. Signed order and
cancel actions use `authorized_signer_address` through the external signer.
Continue with the [testnet operator runbook](hyperliquid-testnet-runbook.md)
after both public addresses have been independently verified.

[mainnet-app]: https://app.hyperliquid.xyz/
[testnet-drip]: https://app.hyperliquid-testnet.xyz/drip
[arbitrum-deposit]: https://hyperliquid.gitbook.io/hyperliquid-docs/support/faq/deposit-or-transfer-issues-missing-lost/deposited-via-arbitrum-network-usdc
[bridge2]: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/bridge2
[account-modes]: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/account-abstraction-modes
[hyperevm-tools]: https://hyperliquid.gitbook.io/hyperliquid-docs/builder-tools/hyperevm-tools
