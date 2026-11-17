# Deskline

Remote desktop with no server to run. Each computer has a code. Paste someone's code, they
accept, you see their screen. Ask for control, they allow, you drive their mouse and keyboard.
Works on the same network or across the internet, the same way PartyLine rooms do.

Linux and Windows.

## Use it

1. Both people open Deskline.
2. The person who needs help clicks **Copy my code** and sends it.
3. The other pastes it under **See a partner's screen** and clicks **Connect**.
4. The first person gets **Screen request**: **Accept**.
   - Linux: the system then asks which screen to share. That answer is remembered, so the next
     sessions only show Deskline's prompt.
5. The viewer clicks **Ask for control**; the sharer clicks **Allow**.
6. The sharer sees a red bar the whole time: **Take back control** or **Stop sharing**.

The code never changes. It's this machine's identity, saved in `~/.config/deskline/identity`
(Linux) or `%APPDATA%\deskline\identity` (Windows). Anyone with that file can pose as this
computer, so don't share the file, only the code.

## Build

```bash
# Linux: needs PipeWire headers, libclang (for the bindings) and nasm (fast encoder)
sudo apt install libpipewire-0.3-dev libclang-dev nasm
cargo build --release            # target/release/deskline

# Windows .exe from Linux (no DLLs needed next to it)
sudo apt install mingw-w64
rustup target add x86_64-pc-windows-gnu
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-C target-feature=+crt-static -L /usr/lib/gcc/x86_64-w64-mingw32/13-posix" \
  cargo build --release --target x86_64-pc-windows-gnu
# -> target/x86_64-pc-windows-gnu/release/deskline.exe
```

On Windows itself, `cargo build --release` works with the MSVC toolchain (install `nasm` for
the fast encoder).

Run two copies on one machine with `--home DIR` (each folder is a separate identity).

## Tests

```bash
cargo test                                         # codec round trip
cargo test two_peers -- --ignored                  # two real endpoints: decline, accept,
                                                   # frames, control, revoke, stop
cargo test --release encode_speed -- --ignored --nocapture   # ms per 1080p frame
```

## How it works

- `src/net.rs`: [iroh](https://github.com/n0-computer/iroh) QUIC dialed by public key (hole
  punching, relay fallback when no direct path exists, mDNS on the LAN). The viewer opens a
  control stream (hello, control requests, input, acks); the sharer opens a video stream. Every
  frame is acked and at most two are in flight, so a slow link skips frames instead of lagging.
- `src/codec.rs`: H.264 with OpenH264 compiled into the binary. Screens wider than 1920 are
  halved. About 8 ms per 1080p frame with nasm.
- `src/desktop/linux.rs`: the xdg RemoteDesktop portal (GNOME, KDE; Wayland or X11) gives the
  screen as a PipeWire stream and accepts input.
- `src/desktop/windows.rs`: DXGI desktop duplication of the primary monitor, `SendInput`.
- `src/ui.rs`: the egui window.

"No server" means no server you run: iroh uses n0's public relays to set up connections and
as a fallback path. Traffic is end-to-end encrypted either way.

## Known limits

- Windows: the mouse pointer isn't drawn in the picture, and admin windows (UAC prompts,
  apps run as admin) ignore input unless Deskline also runs as admin.
- Linux desktops without the RemoteDesktop portal (e.g. XFCE, Cinnamon) can view others but
  can't share.
- Primary monitor only. No clipboard sync, file transfer or audio yet.
- Both sides must run the same version.
