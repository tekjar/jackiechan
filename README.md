# jackiechan

Rust bounded mpsc channel optimized for bulk operations

This is currently a copy of stjepang's awesome async-channel crate with minor 
modifications to support both sync and async without futures_lite.

I'll iteratively modify this to create a new channel crate to fit my usecase
(sharded arenas with lazy commits).

Go use async-channel instead


