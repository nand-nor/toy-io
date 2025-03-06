# Running the Examples

Note: Examples all use `tracing` so to see output you will need to set the `RUST_LOG` env var.

## `block_on`
Example showing the use of `block_on` functionality from the runtime
```
RUST_LOG=debug cargo run --example block_on 
```

## `events`
Example showing the use of the imputio-utils library that provides a simple async event bus actor/handle. 
Optional features include `fairness` and `delay-delete`
```
RUST_LOG=debug cargo run --example events [--features=["fairness"],["delay-delete"]]
```

## `config-events`
Example of advanced configuration options to provide to the runtime builder, extending the `events` example
Optional features include `fairness` and `delay-delete`
```
RUST_LOG=debug cargo run --example config-events [--features=["fairness"],["delay-delete"]]
```

## `config-sockets`
Example of advanced configuration options to provide to the runtime builder when building
a multi-socket TCP server 
Optional features include `fairness` and `delay-delete`
```
RUST_LOG=debug cargo run --example config-sockets [--features=["fairness"],["delay-delete"]]
```

## `simple-server`
Example runs itself by opening a TCP server and client socket that writes some bytes to the socket then exits
```
RUST_LOG=debug cargo run --example simple-server
```

## `tcp-server`
A blocking TCP server, currently only works to send bytes from caller to server
Optional relevant features include `fairness` (although it is unlikely to generate
the creation of enough futures to be noticeable)
```
RUST_LOG=debug cargo run --example tcp-server [--features="fairness"]
```