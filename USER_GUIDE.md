# What is barq?

Barq is a core lightning plugin which can be used by developers to write their own routing strategies without digging 
into the core lightning codebase. If someone wants to implement a new core lightning strategy, or play around with 
existing ones, barq makes it simple for them to do so.


# How to build and install barq?

- clone the repository
`git clone https://github.com/tareknaser/barq.git`

- build project
`make`

- once project is built, you can simply run lightning with the reference to the plugin, or use your preferred way to run a plugin in CLN
    `<path-to-lightning>/lightningd/lightningd ... --plugin=<path-barq-repo>/target/debug/barq-plugin`

# Barq commands

`<path-to-lightning>/cli/lightning-cli --network=<network-name> -k <barq-command> <barq-input-parameter-1> = <value> <barq-input-parameter-2> = <value> ...`

## List of barq commands

- `barqpay` where you can pass the `bolt11_invoice`, `strategy` and `use_rapid_gossip_sync` fields

Example for these commands can be

```lightning-cli --testnet -k barqpay bolt11_invoice=lntb10n1pndpggfsp5sj5zv0espgf2yppax9ne4e0806lwc0n80a673qgd6jwa9g8m4pkspp5ky5x2gkwclr8vzdlpt2hyhx43yhyl8cxw7adzhcdzzkze0qsqhusdq5w3jhxarfdenjqcnpwfcsxqyjw5qcqp2rzjqwyx8nu2hygyvgc02cwdtvuxe0lcxz06qt3lpsldzcdr46my5epmjf5e7uqqqpgqqqqqqqlgqqqqqqgq2q9qxpqysgqsty2zu6hqvl4zfjt40ss0zscjj64s2lpv9a6tqjg5famljcfjylnmcvgmnjeetzuews34lchfhlnmtuycf4f2ls5kzhraws5wglvm5sqr2xuh6```

```clightning --testnet -k barqpay bolt11_invoice=lntb10n1pndqa76sp5hzqyqgljtd6cv2fzgnfwz050v00a69564axg83ztjczkkafqamuqpp577pgggu58vnc0y7v9kdgfs6pu44545w66wfdtdwnsmat6avuy80sdq5w3jhxarfdenjqcnpwfcsxqyjw5qcqp2rzjqgjmhs48xsve8n2e94ascxzmhrrrt8xpm5fn096ud4qn2nj8qwlkg27skgqqqvqqqcqqqp5eqqqqqqsqqc9qxpqysgqdd9twj93z6umjdau7dxdn3f6ts5qnkxy024uqr75dvfa59az8asz4mu5pk939t5u8tvxgzsfr7n0ym7qj8uzwm58c39679jgxc7n3ecq8mkune use_rapid_gossip_sync=true```

```clightning --testnet -k barqpay bolt11_invoice=lntb10n1pndqa76sp5hzqyqgljtd6cv2fzgnfwz050v00a69564axg83ztjczkkafqamuqpp577pgggu58vnc0y7v9kdgfs6pu44545w66wfdtdwnsmat6avuy80sdq5w3jhxarfdenjqcnpwfcsxqyjw5qcqp2rzjqgjmhs48xsve8n2e94ascxzmhrrrt8xpm5fn096ud4qn2nj8qwlkg27skgqqqvqqqcqqqp5eqqqqqqsqqc9qxpqysgqdd9twj93z6umjdau7dxdn3f6ts5qnkxy024uqr75dvfa59az8asz4mu5pk939t5u8tvxgzsfr7n0ym7qj8uzwm58c39679jgxc7n3ecq8mkune strategy=probabilistic```