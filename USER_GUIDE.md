# What is barq?

Barq is a core lightning plugin which can be used by developers to write their own routing strategies without digging 
into the core lightning codebase. If someone wants to implement a new core lightning strategy, or play around with 
existing ones, barq makes it simple for them to do so.


# How to build and install barq?

- clone the repository
`git clone https://github.com/tareknaser/barq.git`

- change directory to barq
`cd barq`

- build project
`cargo build`

- once project is built, you can simply copy ```./target/debug/barq-plugin``` file to ```<path-to-home>/.lightning/plugins``` directory

    `cp target/debug/barq-plugin <path-to-home>/.lightning/plugins`

- Once barq plugin build is copied to plugins directory, simply run lightningd, and you'll have access to all commands provided by barq!

    run lightningd

    `<path-to-lightning>/lightningd/lightningd --network==<network-name>`
    
# Routing strategies in barq

A routing strategy in barq defines the routing algorithm to be used when making payments in the lightning network. Using barq, you can write your own routing strategies.

## How to write a routing strategy

Writing a lightning routing strategy in barq is easy and straightforward! You don't have to worry about digging into core lightning codebase anymore. 

Strategies are defined in barq-common/src/algorithms. You can edit an existing strategy or create a new strategy.

To create a new routing strategy in barq, you can create a new rust file in ```barq-common/src/algorithms```. A strategy is simply an implementation of Strategy trait from ```barq-common/src/strategy.rs```. Create a new implementation of Strategy trait in this new rust file.

You are required to implement two functions, can_apply() and route(). As input, both functions takes RouteInput object, which contains input data required for routing a payment. RouteInput object contains src_pubkey (source public key), dest_pubkey(destination public key), amount_msat (amount to be send), cltv (delay), graph (Network Graph) 

`can_apply()` checks whether the strategy can be applied to a given network graph. You can implement a logic which checks if your strategy can be applied to this network graph.


`route()` returns a possible route in the lightning network. You can implement a logic which finds a possible route through which a lightning payment can be sent.

## Add your strategy to barq

Once you write a strategy, you need to add your strategy to existing list of strategies in barq. 

Go to `barq-plugin/src/plugin.rs`, inside on_init() function, there is a `strategies` vector. Add your strategy to this vector and rebuild barq. 

# Barq commands

To execute a barq functionality, you need to run it's corresponding barq command with core lightning. 

`<path-to-lightning>/cli/lightning-cli --network=<network-name> <barq-command> <barq-input-parameter-1> <barq-input-parameter-2> ...`

## List of barq commands

- barqpay: It takes bolt11 invoice as input, and executes the lightning payment. Returns execution result as response.

`<path-to-lightning>/cli/lightning-cli --network=<network-name> barqpay <bolt11-invoice>`

- barqrouteinfo: It takes destination public key, amount to be sent in msat and cltv (optional) as input, and returns a possible lightning network route through which the corresponding lightning network payment can be sent. 

`<path-to-lightning>/cli/lightning-cli --network=<network-name> barqrouteinfo <destination-public-key> <amount-msat> <cltv>`
