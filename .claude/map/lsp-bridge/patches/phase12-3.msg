feat(examples): save with Ctrl+S in the rust-analyzer example

rust-analyzer's type errors come from `cargo check` on the file on disk, so a fixed error stayed
underlined until something saved the file. Ctrl+S writes the buffer, keeping its line endings,
and sends didSave.
