// Copyright (c) Zefchain Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

/*!
# Fungible Token Example Application

This example application implements fungible tokens. This demonstrates in particular
cross-chain messages and how applications are instantiated and auto-deployed.

Once this application is built and its bytecode published on a Linera chain, the
published bytecode can be used to create multiple application instances, where each
instance represents a different fungible token.

# How It Works

Individual chains have a set of accounts, where each account has an owner and a balance. The
same owner can have accounts on multiple chains, with a different balance on each chain. This
means that an account's balance is sharded across one or more chains.

There are two operations: `Transfer` and `Claim`. `Transfer` sends tokens from an account on the
chain where the operation is executed, while `Claim` sends a message from the current chain to
another chain in order to transfer tokens from that remote chain.

Tokens can be transferred from an account to different destinations, such as:

- other accounts on the same chain,
- the same account on another chain,
- other accounts on other chains,
- sessions so that other applications can use some tokens.

# Usage

## Setting Up

The WebAssembly binaries for the bytecode can be built and published using [steps from the
book](https://linera-io.github.io/linera-documentation/getting_started/first_app.html),
summarized below.

Before getting started, make sure that the binary tools `linera*` corresponding to
your version of `linera-sdk` are in your PATH. For scripting purposes, we also assume
that the BASH function `linera_spawn_and_read_wallet_variables` is defined.

From the root of Linera repository, this can be achieved as follows:

```bash
export PATH="$PWD/target/debug:$PATH"
source /dev/stdin <<<"$(linera net helper 2>/dev/null)"
```

You may also use `cargo install linera-service` and append the output of
`linera net helper` to your `~/.bash_profile`.

Now, we are ready to set up a local network with an initial wallet owning several initial
chains. In a new BASH shell, enter:

```bash
linera_spawn_and_read_wallet_variables linera net up --testing-prng-seed 37
```

A new test network is now running and the environment variables `LINERA_WALLET` and
`LINERA_STORAGE` are now be defined for the duration of the shell session. We used the
test-only CLI option `--testing-prng-seed` to make keys deterministic and simplify our
presentation.

Now, compile the `fungible` application WebAssembly binaries, and publish them as an application
bytecode:

```bash
(cd examples/fungible && cargo build --release)

BYTECODE_ID=$(linera publish-bytecode \
    examples/target/wasm32-unknown-unknown/release/fungible_{contract,service}.wasm)
```

Here, we stored the new bytecode ID in a variable `BYTECODE_ID` to be reused it later.

## Creating a Token

In order to use the published bytecode to create a token application, the initial state must be
specified. This initial state is where the tokens are minted. After the token is created, no
additional tokens can be minted and added to the application. The initial state is a JSON string
that specifies the accounts that start with tokens.

In order to select the accounts to have initial tokens, the command below can be used to list
the chains created for the test in the default wallet:

```bash
linera wallet show
```

A table will be shown with the chains registered in the wallet and their meta-data. The default
chain should be highlighted in green. Each chain has an `Owner` field, and that is what is used
for the account.

Let's define some variables corresponding to these values. (Note that owner addresses
would not be predictable without `--testing-prng-seed` above.)

```bash
CHAIN_1=e476187f6ddfeb9d588c7b45d3df334d5501d6499b3f9ad5595cae86cce16a65  # default chain for the wallet
OWNER_1=e814a7bdae091daf4a110ef5340396998e538c47c6e7d101027a225523985316  # owner of chain 1
CHAIN_2=256e1dbc00482ddd619c293cc0df94d366afe7980022bb22d99e33036fd465dd  # another chain in the wallet
OWNER_2=0d677b87f1bc6e12442f98c06b9c105fbebf6bb21d885739e23c315956c7d7f3  # owner of chain 2
```

The example below creates a token application on the default chain CHAIN_1 and gives the owner 100 tokens:

```bash
APP_ID=$(linera create-application $BYTECODE_ID \
    --json-argument "{ \"accounts\": {
        \"User:$OWNER_1\": \"100.\"
    } }" \
)
```

This will store the application ID in a new variable `APP_ID`.

## Using the Token Application

Before using the token, a source and target address should be selected. The source address
should ideally be on the default chain (used to create the token) and one of the accounts chosen
for the initial state, because it will already have some initial tokens to send.

First, a node service for the current wallet has to be started:

```bash
PORT=8080
linera service --port $PORT &
```

Then the web frontend:

```bash
cd examples/fungible/web-frontend
npm install

# Start the server but not open the web page right away.
BROWSER=none npm start &
```

Web UIs for specific accounts can be opened by navigating URLs of the form
`http://localhost:3000/$CHAIN?app=$APP_ID&owner=$OWNER&port=$PORT` where
- the path is the ID of the chain where the account is located.
- the argument `app` is the token application ID obtained when creating the token.
- `owner` is the address of the chosen user account (owner must be have permissions to create blocks in the given chain).
- `port` is the port of the wallet service (the wallet must know the secret key of `owner`).

In this example, two web pages for OWNER_1 and OWNER_2 can be opened by navigating these URLs:

```bash
echo "http://localhost:3000/$CHAIN_1?app=$APP_ID&owner=$OWNER_1&port=$PORT"
echo "http://localhost:3000/$CHAIN_1?app=$APP_ID&owner=$OWNER_2&port=$PORT"
```

OWNER_2 doesn't have the applications loaded initially. Using the first page to
transfer tokens from OWNER_1 to OWNER_2 at CHAIN_2 will instantly update the UI of the
second page.
*/

use async_graphql::{scalar, InputObject, Request, Response};
use linera_sdk::{
    base::{Amount, ApplicationId, ChainId, ContractAbi, Owner, ServiceAbi},
    graphql::GraphQLMutationRoot,
};
use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeMap, str::FromStr};
#[cfg(all(any(test, feature = "test"), not(target_arch = "wasm32")))]
use {
    async_graphql::InputType,
    futures::{stream, StreamExt},
    linera_sdk::{
        base::BytecodeId,
        test::{ActiveChain, TestValidator},
    },
};

pub struct FungibleTokenAbi;

impl ContractAbi for FungibleTokenAbi {
    type InitializationArgument = InitialState;
    type Parameters = ();
    type ApplicationCall = ApplicationCall;
    type Operation = Operation;
    type Message = Message;
    type SessionCall = SessionCall;
    type Response = Amount;
    type SessionState = Amount;
}

impl ServiceAbi for FungibleTokenAbi {
    type Query = Request;
    type QueryResponse = Response;
    type Parameters = ();
}

/// An operation.
#[derive(Debug, Deserialize, Serialize, GraphQLMutationRoot)]
pub enum Operation {
    /// A transfer from a (locally owned) account to a (possibly remote) account.
    Transfer {
        owner: AccountOwner,
        amount: Amount,
        target_account: Account,
    },
    /// Same as transfer but the source account may be remote. Depending on its
    /// configuration (see also #464), the target chain may take time or refuse to process
    /// the message.
    Claim {
        source_account: Account,
        amount: Amount,
        target_account: Account,
    },
}

/// A message.
#[derive(Debug, Deserialize, Serialize)]
pub enum Message {
    /// Credit the given account.
    Credit { owner: AccountOwner, amount: Amount },

    /// Withdraw from the given account and starts a transfer to the target account.
    Withdraw {
        owner: AccountOwner,
        amount: Amount,
        target_account: Account,
    },
}

/// A cross-application call.
#[derive(Debug, Deserialize, Serialize)]
pub enum ApplicationCall {
    /// A request for an account balance.
    Balance { owner: AccountOwner },
    /// A transfer from an account.
    Transfer {
        owner: AccountOwner,
        amount: Amount,
        destination: Destination,
    },
    /// Same as transfer but the source account may be remote.
    Claim {
        source_account: Account,
        amount: Amount,
        target_account: Account,
    },
}

/// A cross-application call into a session.
#[derive(Debug, Deserialize, Serialize)]
pub enum SessionCall {
    /// A request for the session's balance.
    Balance,
    /// A transfer from the session.
    Transfer {
        amount: Amount,
        destination: Destination,
    },
}

scalar!(AccountOwner);

/// An account owner.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AccountOwner {
    /// An account owned by a user.
    User(Owner),
    /// An account for an application.
    Application(ApplicationId),
}

impl Serialize for AccountOwner {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            AccountOwner::User(owner) => {
                let key = format!("User:{}", owner);
                serializer.serialize_str(&key)
            }
            AccountOwner::Application(app_id) => {
                let key = format!("Application:{}", app_id);
                serializer.serialize_str(&key)
            }
        }
    }
}

impl<'de> Deserialize<'de> for AccountOwner {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AccountOwnerVisitor;

        impl<'de> serde::de::Visitor<'de> for AccountOwnerVisitor {
            type Value = AccountOwner;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string representing an AccountOwner")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let parts = value.splitn(2, ':').collect::<Vec<_>>();
                if parts.len() != 2 {
                    return Err(Error::custom("string does not contain colon"));
                }

                match parts[0] {
                    "User" => {
                        let owner = Owner::from_str(parts[1]).map_err(|_| {
                            Error::custom(format!(
                                "failed to parse Owner from string: {}",
                                parts[1]
                            ))
                        })?;
                        Ok(AccountOwner::User(owner))
                    }
                    "Application" => {
                        let app_id = ApplicationId::from_str(parts[1]).map_err(|_| {
                            Error::custom(format!(
                                "failed to parse ApplicationId from string: {}",
                                parts[1]
                            ))
                        })?;
                        Ok(AccountOwner::Application(app_id))
                    }
                    _ => Err(Error::unknown_variant(parts[0], &["User", "Application"])),
                }
            }
        }
        deserializer.deserialize_str(AccountOwnerVisitor)
    }
}

impl<T> From<T> for AccountOwner
where
    T: Into<Owner>,
{
    fn from(owner: T) -> Self {
        AccountOwner::User(owner.into())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct InitialState {
    pub accounts: BTreeMap<AccountOwner, Amount>,
}

/// An account.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, InputObject,
)]
pub struct Account {
    pub chain_id: ChainId,
    pub owner: AccountOwner,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Destination {
    Account(Account),
    NewSession,
}

scalar!(Destination);

/// A builder type for constructing the initial state of the application.
#[derive(Debug, Default)]
pub struct InitialStateBuilder {
    account_balances: BTreeMap<AccountOwner, Amount>,
}

impl InitialStateBuilder {
    /// Adds an account to the initial state of the application.
    pub fn with_account(mut self, account: AccountOwner, balance: impl Into<Amount>) -> Self {
        self.account_balances.insert(account, balance.into());
        self
    }

    /// Returns the serialized initial state of the application, ready to used as the
    /// initialization argument.
    pub fn build(&self) -> InitialState {
        InitialState {
            accounts: self.account_balances.clone(),
        }
    }
}

#[cfg(all(any(test, feature = "test"), not(target_arch = "wasm32")))]
impl FungibleTokenAbi {
    /// Creates a fungible token application and distributes `initial_amounts` to new individual
    /// chains.
    pub async fn create_with_accounts(
        validator: &TestValidator,
        bytecode_id: BytecodeId<Self>,
        initial_amounts: impl IntoIterator<Item = Amount>,
    ) -> (
        ApplicationId<Self>,
        Vec<(ActiveChain, AccountOwner, Amount)>,
    ) {
        let mut token_chain = validator.new_chain().await;
        let mut initial_state = InitialStateBuilder::default();

        let accounts = stream::iter(initial_amounts)
            .then(|initial_amount| async move {
                let chain = validator.new_chain().await;
                let account = AccountOwner::from(chain.public_key());

                (chain, account, initial_amount)
            })
            .collect::<Vec<_>>()
            .await;

        for (_chain, account, initial_amount) in &accounts {
            initial_state = initial_state.with_account(*account, *initial_amount);
        }

        let application_id = token_chain
            .create_application(bytecode_id, (), initial_state.build(), vec![])
            .await;

        for (chain, account, initial_amount) in &accounts {
            chain.register_application(application_id).await;

            let claim_messages = chain
                .add_block(|block| {
                    block.with_operation(
                        application_id,
                        Operation::Claim {
                            source_account: Account {
                                chain_id: token_chain.id(),
                                owner: *account,
                            },
                            amount: *initial_amount,
                            target_account: Account {
                                chain_id: chain.id(),
                                owner: *account,
                            },
                        },
                    );
                })
                .await;

            assert_eq!(claim_messages.len(), 2);

            let transfer_messages = token_chain
                .add_block(|block| {
                    block.with_incoming_message(claim_messages[1]);
                })
                .await;

            assert_eq!(transfer_messages.len(), 2);

            chain
                .add_block(|block| {
                    block.with_incoming_message(transfer_messages[1]);
                })
                .await;
        }

        (application_id, accounts)
    }

    /// Queries the balance of an account owned by `account_owner` on a specific `chain`.
    pub async fn query_account(
        application_id: ApplicationId<FungibleTokenAbi>,
        chain: &ActiveChain,
        account_owner: AccountOwner,
    ) -> Option<Amount> {
        let query = format!(
            "query {{ accounts(accountOwner: {}) }}",
            account_owner.to_value()
        );
        let value = chain.graphql_query(application_id, query).await;
        let balance = value.as_object()?.get("accounts")?.as_str()?;

        Some(
            balance
                .parse()
                .expect("Account balance cannot be parsed as a number"),
        )
    }
}
