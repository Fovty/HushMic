# smithay-clipboard 0.7.3, patched for drops

This is [smithay-clipboard](https://github.com/smithay/smithay-clipboard) 0.7.3 as published on
crates.io (MIT, see `LICENSE`), with one change. The workspace's `Cargo.toml` uses it in place of
the published crate through `[patch.crates-io]`.

**What changed:** the clipboard's Wayland data device also takes drag-and-drop. `src/dnd.rs`
(new) holds a handler that the application sets with `dnd::set_handler(mimes, handler)`. The
`DataDeviceHandler` and `DataOfferHandler` in `src/state.rs` accept a drag that offers one of
those MIME types (with the copy action), read the drop off-thread, call the handler, and then
finish the offer. The handler runs before the finish because a FileTransfer portal key stops
working once the source hears the drop is finished. The handler is called outside the lock that
holds it, so a drop waiting on its data never holds up the device's own thread (or paste). Without
a handler, nothing changes: no drop is accepted, as in 0.7.3. `dnd::testing` (hidden) exposes the
device's two calls to HushMic's tests.

**Why:** winit 0.30 takes no drops on Wayland, so HushMic's window needs a data device of its
own. A second device next to the clipboard's does not work everywhere. KWin (Plasma 6) delivers
drags to a client's first device only. Mutter (GNOME) delivers the selection to its newest
device only, which breaks pasting into the window. The clipboard's own device is the one every
compositor serves.

Kept as a local patch; drop it if upstream ever adds drag-and-drop. To drop it, remove the
`[patch.crates-io]` entry and this directory, and move `crates/hushmic/src/window/dnd.rs` onto
upstream's API.
