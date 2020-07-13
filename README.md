## s2n-quic

TODO: Fill this README out!

## Installation

`s2n-quic` is available on `crates.io` and can be added to a project like so:

```toml
[dependencies]
s2n-quic = "1"
```

## Security

See [CONTRIBUTING](CONTRIBUTING.md#security-issue-notifications) for more information.

## License

This project is licensed under the Apache-2.0 License.

## Development

### Prerequisites

- Install [rustup](https://rustup.rs/)
- Run `rustup component add rustfmt clippy rls rust-analysis`

### Running a fuzz target

You'll need to have `cargo-bolero` installed first:

```bash
$ cargo install cargo-bolero --force
```

```bash
$ cargo bolero fuzz varint -p s2n-quic-core -s address
```

Fuzz targets can be executed on stable by removing the sanitzer flag:

```bash
$ cargo bolero fuzz varint -p s2n-quic-core
```