<div align="center">
  <h1><code>tedbot-rs</code></h1>
</div>

## Usage

### Development

- Setup a `.env` file using the example `example.env`.
- Use `--features dotenv` when building or running.
- Use the Cargo alias `cargo w` to watch for changes, build, and run the discord bot (requires
  [`cargo-watch`](https://lib.rs/crates/cargo-watch)).

### Production (`--release`)

- Set environment variables using the example `example.env`.
- Build with no features (default behavior).
