# Bolt CLI

The Bolt CLI is a collection of command-line tools for interacting with Bolt protocol.

## Installation

Prerequisites:

- [Rust toolchain][rust]
- [Protoc][protoc]

Once you have the necessary prerequisites, you can build the binary in the following way:

```shell
# clone the Bolt repository if you haven't already
git clone git@github.com:chainbound/bolt.git

# navigate to the Bolt CLI package directory
cd bolt-cli

# build and install the binary on your machine
cargo install --path . --force

# test the installation
bolt --version
```

## Usage

Available commands:

- [`delegate`](#delegate) - Generate BLS delegation messages for the Constraints API.
- [`pubkeys`](#pubkeys) - List available BLS public keys from various key sources.
- [`send`](#send) - Send a preconfirmation request to a Bolt sidecar.

---

### `Delegate`

The `delegate` command generates signed delegation messages for the Constraints API.
To learn more about the Constraints API, please refer to the [Bolt documentation][bolt-docs].

The `delegate` command supports three key sources:

- Local BLS secret keys (as hex-encoded strings) via `secret-keys`
- Local EIP-2335 filesystem keystore directories via `local-keystore`
- Remote Dirk keystore via `dirk` (requires TLS credentials)

<details>
<summary>Usage</summary>

```text
❯ bolt delegate --help

Generate BLS delegation or revocation messages

Usage: bolt delegate [OPTIONS] --delegatee-pubkey <DELEGATEE_PUBKEY> <COMMAND>

Commands:
secret-keys     Use local secret keys to generate the signed messages
local-keystore  Use an EIP-2335 filesystem keystore directory to generate the signed messages
dirk            Use a remote DIRK keystore to generate the signed messages
help            Print this message or the help of the given subcommand(s)

Options:
    --delegatee-pubkey <DELEGATEE_PUBKEY>
        The BLS public key to which the delegation message should be signed

        [env: DELEGATEE_PUBKEY=]

    --out <OUT>
        The output file for the delegations

        [env: OUTPUT_FILE_PATH=]
        [default: delegations.json]

    --chain <CHAIN>
        The chain for which the delegation message is intended

        [env: CHAIN=]
        [default: mainnet]
        [possible values: mainnet, holesky, helder, kurtosis]

    --action <ACTION>
        The action to perform. The tool can be used to generate delegation or revocation messages (default: delegate)

        [env: ACTION=]
        [default: delegate]

        Possible values:
        - delegate: Create a delegation message
        - revoke:   Create a revocation message

-h, --help
        Print help (see a summary with '-h')
```

</details>

<details>
<summary>Examples</summary>

1. Generating a delegation using a local BLS secret key

```text
bolt delegate \
  --delegatee-pubkey 0x8d0edf4fe9c80cd640220ca7a68a48efcbc56a13536d6b274bf3719befaffa13688ebee9f37414b3dddc8c7e77233ce8 \
  --chain holesky \
  secret-keys --secret-keys 642e0d33fde8968a48b5f560c1b20143eb82036c1aa6c7f4adc4beed919a22e3
```

2. Generating a delegation using an ERC-2335 keystore directory

```text
bolt delegate \
 --delegatee-pubkey 0x8d0edf4fe9c80cd640220ca7a68a48efcbc56a13536d6b274bf3719befaffa13688ebee9f37414b3dddc8c7e77233ce8 \
 --chain holesky \
 local-keystore --path test_data/lighthouse/validators --password-path test_data/lighthouse/secrets
```

3. Generating a delegation using a remote DIRK keystore

```text
bolt delegate \
  --delegatee-pubkey 0x83eeddfac5e60f8fe607ee8713efb8877c295ad9f8ca075f4d8f6f2ae241a30dd57f78f6f3863a9fe0d5b5db9d550b93 \
  --chain holesky \
  dirk --url https://localhost:9091 \
  --client-cert-path ./test_data/dirk/client1.crt \
  --client-key-path ./test_data/dirk/client1.key \
  --ca-cert-path ./test_data/dirk/security/ca.crt \
  --wallet-path wallet1 --passphrases secret
```

</details>

---

### `Pubkeys`

The `pubkeys` command lists available BLS public keys from different key sources:

- Local BLS secret keys (as hex-encoded strings) via `secret-keys`
- Local EIP-2335 filesystem keystore directories via `local-keystore`
- Remote Dirk keystore via `dirk` (requires TLS credentials)

<details>
<summary>Usage</summary>

```text
❯ bolt pubkeys --help

Output a list of pubkeys in JSON format

Usage: bolt pubkeys [OPTIONS] <COMMAND>

Commands:
  secret-keys     Use local secret keys to generate the signed messages
  local-keystore  Use an EIP-2335 filesystem keystore directory to generate the signed messages
  dirk            Use a remote DIRK keystore to generate the signed messages
  help            Print this message or the help of the given subcommand(s)

Options:
      --out <OUT>  The output file for the pubkeys [env: OUTPUT_FILE_PATH=] [default: pubkeys.json]
  -h, --help       Print help
```

</details>

<details>
<summary>Examples</summary>

1. Listing BLS public keys from a local secret key

```text
bolt pubkeys secret-keys --secret-keys 642e0d33fde8968a48b5f560c1b20143eb82036c1aa6c7f4adc4beed919a22e3
```

2. Listing BLS public keys from an ERC-2335 keystore directory

```text
bolt pubkeys local-keystore \
  --path test_data/lighthouse/validators \
  --password-path test_data/lighthouse/secrets
```

3. Listing BLS public keys from a remote DIRK keystore

```text
bolt pubkeys dirk --url https://localhost:9091 \
  --client-cert-path ./test_data/dirk/client1.crt \
  --client-key-path ./test_data/dirk/client1.key \
  --ca-cert-path ./test_data/dirk/security/ca.crt \
  --wallet-path wallet1 --passphrases secret
```

</details>

---

### `Send`

The `send` command sends a preconfirmation request to a Bolt sidecar.

<details>
<summary>Usage</summary>

```text
❯ bolt send --help

Send a preconfirmation request to a Bolt proposer

Usage: bolt send [OPTIONS] --private-key <PRIVATE_KEY>

Options:
      --bolt-rpc-url <BOLT_RPC_URL>
          Bolt RPC URL to send requests to and fetch lookahead info from

          [env: BOLT_RPC_URL=]
          [default: http://135.181.191.125:58017/rpc]

      --private-key <PRIVATE_KEY>
          The private key to sign the transaction with

          [env: PRIVATE_KEY]

      --override-bolt-sidecar-url <OVERRIDE_BOLT_SIDECAR_URL>
          The Bolt Sidecar URL to send requests to. If provided, this will override the canonical bolt RPC URL and disregard any registration information.

          This is useful for testing and development purposes.

          [env: OVERRIDE_BOLT_SIDECAR_URL=]

      --count <COUNT>
          How many transactions to send

          [env: TRANSACTION_COUNT=]
          [default: 1]

      --blob
          If set, the transaction will be blob-carrying (type 3)

          [env: BLOB=]

  -h, --help
          Print help (see a summary with '-h')
```

</details>

<details>
<summary>Examples</summary>

1. Sending a preconfirmation request to a Bolt sidecar

```text
bolt send --private-key $(openssl rand -hex 32)
```

</details>

---

## Security

The Bolt CLI is designed to be used offline. It does not require any network connections
unless you are using the remote `dirk` key source. In that case, the tool will connect to
the Dirk server with the provided TLS credentials.

The tool does not store any sensitive information beyond the duration of the execution.
It is recommended to use the tool in a secure environment and to avoid storing any sensitive
information in the shell history.

If you have any security concerns or have found a security issue/bug, please contact Chainbound
on our official [Discord][discord] or [Twitter][twitter] channels.

<!-- Links -->

[rust]: https://www.rust-lang.org/tools/install
[protoc]: https://grpc.io/docs/protoc-installation/
[bolt-docs]: https://docs.boltprotocol.xyz/
[discord]: https://discord.gg/G5BJjCD9ss
[twitter]: https://twitter.com/chainbound_
