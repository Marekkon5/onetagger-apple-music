# onetagger-apple-music

This is Apple Music custom platform implementation for Apple Music.
It is not included in base OneTagger, because of several reasons:

- Not user friendly
- Requires the user to manually obtain the `media_user_token` from headers
- Gray area
- Requires a paid / premium account

### How to install:

From Actions build or local `target/release/` copy:
- on Linux: `.so` file
- on Window: `.dll` file
- on MacOS: `.dylib` file

to: 
- on Linux: `~/.config/onetagger/platforms`
- on Window: `%appdata%\OneTagger\OneTagger\platforms`
- on MacOS: `/Users/your-user-account/Library/Preferences/com.OneTagger.OneTagger/platforms`


### How to build

0. Install Rust: [rustup.rs](https://rustup.rs)
1. Clone the repo
2. `cargo build --release`