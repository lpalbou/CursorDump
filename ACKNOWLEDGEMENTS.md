# Acknowledgements

CursorDump builds on excellent open-source work:

- **[axum](https://github.com/tokio-rs/axum)** and
  **[tokio](https://tokio.rs)** — the local web server and async runtime.
- **[serde](https://serde.rs)** / **serde_json** — transcript and manifest
  (de)serialization.
- **[regex](https://github.com/rust-lang/regex)**, **sha2**, **dirs**,
  **anyhow**, **open**, **tokio-util** — parsing, hashing, platform paths,
  error handling, browser launch, and streaming.

The dataset formats follow the conventions of the
[HuggingFace `datasets`](https://github.com/huggingface/datasets) ecosystem,
[Unsloth](https://unsloth.ai)'s fine-tuning guides (ChatML and ShareGPT
schemas, CPT `text` corpus), and
[ForgeLLM](https://github.com/lpalbou/ForgeLLM)'s plain-text dataset layout.

Thanks to the [Cursor](https://cursor.com) team for an agent-native IDE whose
on-disk transcripts make this kind of tooling possible. CursorDump is an
independent project and is not affiliated with or endorsed by Cursor.
