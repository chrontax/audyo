# Audyo

Audio decoding/encoding library with a simple api.

## Features

- audio decoding (currently supports all formats supported by [symphonia](https://crates.io/crates/symphonia))
- audio encoding (currently only supports ogg vorbis)

## Usage

```none
cargo add audyo
```

```rust
let decoded = audyo::decode::<f32>(File::open("uwu.mp3").unwrap()).unwrap();

let vorbis_encoded = audyo::encode_vorbis(&decoded.1, 320000).unwrap();
```
