# Blighted client patch (BlightedNewWorld.exe)

This is the authoritative procedure for making our patched client. Keep it
current. The emulator cannot be entered by the stock client without it.

## What and why

The real New World client trusts only a certificate embedded in
`NewWorld.exe`, and we do not have its private key. So the client refuses
the emulator gateway's self-signed certificate and fails world entry with
`@mm_csdkerr_transport_security_error`. We cannot select the client's
no-certificate transport by config either (the retail client reads no
external override).

The fix is one small change to the client: stop it verifying the server
certificate. Then its normal `gridmate-udp` DTLS connection accepts the
emulator gateway, and the whole UDP/DTLS path is reused.

We never modify the official `NewWorld.exe`. We make a **separate patched
copy**, `BlightedNewWorld.exe`, next to it:

- Retail play through Steam uses the untouched `NewWorld.exe`.
- Our launcher runs `BlightedNewWorld.exe` directly (so Easy Anti-Cheat,
  started by `NewWorldLauncher.exe`, never runs).

## Exactly what is changed

One function, OpenSSL `SSL_CTX_set_verify(ctx, mode, callback)`:

```text
MOV dword ptr [RCX + 0x158], EDX    89 91 58 01 00 00   ; ctx->verify_mode  = mode
MOV qword ptr [RCX + 0x188], R8     4C 89 81 88 01 00 00 ; ctx->verify_callback = callback
RET                                 C3
```

The patch replaces the first instruction (the `verify_mode` write) with six
`NOP` bytes:

```text
before:  89 91 58 01 00 00
after:   90 90 90 90 90 90
```

A fresh `SSL_CTX` starts with `verify_mode = 0` (`SSL_VERIFY_NONE`). Not
writing the mode leaves it at 0, so the client verifies no certificate. With
mode 0 OpenSSL never calls the verify callback, so leaving the callback write
in place is harmless. The change is six bytes and does not change the file
length.

This setter is shared, so after the patch no TLS connection in the client
verifies the server certificate. That is acceptable: the control plane
(`edge-http`) presents a certificate the client already trusts, so making it
more permissive changes nothing there, and the game-server path is exactly
what we want to accept. The patch only weakens our own client for connecting
to our own server. **Restore the official exe before playing on any real or
official server.**

Build where this was derived: Steam build `25810808`, patch at file offset
`0x78F0B90`. Reasoning and the reverse-engineering trail:
[docs/re/2026-09-06-cert-bypass-patch-plan.md](re/2026-09-06-cert-bypass-patch-plan.md).

## Second change: Exit to desktop

Without it, "Exit to desktop" hangs the client. The exit path calls the Steam
auth manager's `EndSession`, which only stores the quit callback and asks the
Omni persona client to finish later. Our setup has no persona client, so the
callback never runs and the game never quits.

The Sonic auth manager's `EndSession` runs the callback at once (the callback
logs `In callback function for IAuthManager::GetInstance()->EndSession()` and
runs the console command `quit`). The patch points the Steam `EndSession`
vtable slot at the Sonic function: one 8-byte pointer in `.rdata`, in build
25810808 at file offset `0x844AB78`, `0x145A8D380` -> `0x145A8D310`. The Sonic
function also writes 1 to the byte at `this+0x58`; no Steam or base
constructor sets that byte, and the write comes after the quit has started.

The tool finds both functions by byte signatures (the Steam one with a
wildcard for its call offset) and then the single pointer to the Steam
function. Reasoning and addresses:
[docs/re/2026-09-10-exit-to-desktop-patch.md](re/2026-09-10-exit-to-desktop-patch.md).

"Exit to main menu" is a different path and is not changed by this patch.

## Third change: offline Steam ticket

Without it, the game cannot log in while the Steam client is offline (for
example after the PC slept and Steam lost its connection). `Game.log` stops at
`[SteamAuth] Getting steam auth session ticket.` and the game stays at "press
any button". With Steam online the ticket comes in about 1 s and the log goes
on with `OnAuthSessionTicket: Authenticating using steam auth session ticket
handle: ...` and `OnAuthSessionTicket: Calling Auth stack!`.

The Steam auth manager's `GetTicket` (build 25810808: `FUN_145aa0d10`; its
handler is `FUN_145a94820`) does this after the log line:

```text
CMP  dword ptr [RBX+0x2F0], 0           ; an old ticket handle?
...  ISteamUser::CancelAuthTicket(old)  ; vtable +0x88, then clear the old ticket
...  ISteamUser::GetAuthTicketForWebApi(NULL)   ; vtable +0x70
MOV  dword ptr [RBX+0x2F0], EAX         ; the new handle
```

The ticket itself comes later in the Steam callback
`GetTicketForWebApiResponse_t` (handle, result, length, 2,560 ticket bytes).
The Steam callback object at manager `+0x1A8` enters the handler through the
thunk at `0x145A9F750` (`ADD RCX,-0x1A8` / `JMP 0x145A94820`), so the handler
gets the manager in `RCX` and the response in `RDX`. The handler ignores a
response whose handle is not the stored one, reports a Steam error for a result
other than OK, copies the ticket to `this+0x38C`, and in the `nw` backend posts
`{"steamAuthTicket":"<hex>","steamAppId":"...","branch":"..."}` to our
`/steam` route. While the Steam client is offline Steam never sends the
callback.

Our `/steam` route (`crates/nw-login/src/game.rs`, `steam_auth`) does not read
the body: the account comes from the launcher session in the path. So the
ticket has no use for us. The patch replaces the request (106 bytes, from the
`CMP` after the log call to the `RET`) with a direct handler call:

```text
MOV  dword ptr [RBX+0x2F0], 0     ; handle 0 (k_HAuthTicketInvalid)
MOV  dword ptr [RSP+0x30], 0      ; response.handle = 0
MOV  dword ptr [RSP+0x34], 1      ; response.result = k_EResultOK
MOV  dword ptr [RSP+0x38], 0      ; response.ticket length = 0
LEA  RDX, [RSP+0x30]
MOV  RCX, RBX
CALL OnAuthSessionTicket          ; FUN_145a94820
ADD  RSP, 0x20 / POP RBX / RET
INT3 ...                          ; to the end of the replaced bytes
```

The response sits in `GetTicket`'s home space; the handler saves registers only
in its own home space, below it. With length 0 the handler copies no bytes and
sends the ticket `""` (the log shows `ticket: steam|`). The game no longer asks
Steam for a ticket at all, so the login is the same whether Steam is online or
offline.

Limit: the Steam client must still run with a user logged in (online or
offline). The game calls `SteamAPI_Init` at start and shows "Steam must be
running to play this game." when it fails; it also asks Steam for the app ID
(`ISteamUtils`, local) for the `/steam` body. This patch does not change that.

The tool finds `GetTicket` and the handler by byte signatures (with wildcards
for RIP-relative and call offsets) and writes the handler's address into the
`CALL`.

## Fourth change: own-movement correction

The client moves its own player at once from the keys (prediction) and then
compares that with the own place the server sends for the same input number.
Eight console variables steer that correction. The game settings constructor
writes their defaults (build 25810808: `MOV dword ptr [RDI + field], imm32` at
`0x140FEC21A`), and `FUN_141031800` registers them:

| Field | Variable (`g_localPlayer_...`) | Default | Patched |
|---|---|---|---|
| `0x2F0` | `CorrectionThreshold` | 0.01 m | **100,000 m** |
| `0x2F4` | `IdleVerticalCorrectionThreshold` | 0.5 m | |
| `0x2F8` | `IdleHorizontalCorrectionThreshold` | 0.3 m | |
| `0x2FC` | `AddedErrorDistancePerMissedServerState` | 0.4 m | |
| `0x300` | `AddedErrorDistanceMaxMissedServerStates` | 5 | |
| `0x304` | `ErrorDistanceForFullCorrection` | 1.0 m | **100,000 m** |
| `0x308` | `FailedCorrectionsBeforeSnap` | 4 | |
| `0x30C` | `RequiredPhysicalCorrectionFraction` | 0.5 | |

The variable help texts: no correction is applied while the total correction
is less than `CorrectionThreshold`; past `ErrorDistanceForFullCorrection` a
full correction (likely a snap) happens.

Why: retail's server runs the client's own character controller, so its own
place is where the client stands. The emulator only estimates the body from
the client's camera. With the retail defaults the client corrected itself on
nearly every state (run 16); with 0.5 m and 2.0 m it still did, and each
correction moved the camera the estimate follows, so the loop pulled the
player back and sideways (run 21, 2026-09-26: steps of 0.24 m forward and back
each frame). The closed-loop replay of the retail tutorial (`content-tool`
test `own_replay`, feature `own-replay`) counts 5,811 pulls with 0.5 m and
2.0 m, 1,499 of them against the player's motion, and none with these values.
Past any level's size, the client never corrects its own movement: it owns it.

The own place is still sent in every own state: without it (run 22) the client
could not move on the ship and hung in the air after the drop into the sea.
Server moves are teleports, which the client takes by their own step, not by
this correction (the drop of run 22 moved it while the states carried no
place). The server still keeps the body that other players see and that every
rule uses.

The tool finds the eight writes by their exact bytes and changes the two
immediates. Re-make the copy after a change of these values
(`python3 scripts/patch-client.py "<game>/Bin64"`).

## Typed chat

The client holds typed chat back until a privacy check answers. The check is the
console variable `javelin.social.enable-vpc-restrictions` (default on); its answer
comes from a platform service the emulator setup does not have, so the client
never sends a typed line (live 2026-11-01: no 1291 reached the sim or `social`,
while `nw-testclient chat` worked). The settings function (build 25810808:
`FUN_14616d7b0`) creates each variable as `LEA RDX,[name]` / `LEA RCX,[RBP+var]` /
`CALL` / `MOV R8B,<default>` / `LEA RDX,[RBP+var]`. The tool finds the one
creation whose `LEA RDX` points at the variable's name and makes the default 0
(build 25810808: file offset `0x616E0D4`, VA `0x14616EAD4`). The same change, at
another place, turns typed chat on in another build of the client (Inferred: the
same check). The account-age chat restriction stays on; the server answers it with
`accountInProbation`.

## Fifth change: AWS proxy (Windows only, `--aws-proxy`)

This change is for Windows, where there is no `bwrap` sandbox to give the game
process a private hosts file and trust file. On Linux leave it off.

The game has three AWS clients that call fixed Amazon names with certificate
checks on: remote-config (S3), analytics (Kinesis) and analytics credentials
(STS). Each builds an `Aws::Client::ClientConfiguration` (constructor
`FUN_147467a40`) and uses the WinHTTP client (`FUN_147491570`; the factory
`FUN_146b73730` picks WinINet only when the configuration byte `+0x220` is `2`,
and the constructor sets it to `0`). With `--aws-proxy` the patch sends all
three through the launcher's loopback proxy instead
(`docs/re/2026-09-20-windows-launcher-network.md`).

After each builder's constructor call (the fresh config is in `RAX`), the patch
replaces the following `NOP` and one instruction with a jump to a code cave in
the `.text` INT3 padding. The cave sets, through `RAX`:

```text
proxyHost  +0xA0  = "127.0.0.1"   (MSVC std::string: inline buffer, size 9 at +0xB0)
proxyPort  +0xC0  = 47443         (AWS_PROXY_PORT; bins/launcher/src/aws_proxy.rs uses the same)
verifySSL  +0x1B8 = 0
```

then replays the instruction the jump replaced and jumps back. The remote-config
builder also writes `verifySSL = 1` itself a little later
(`MOV byte ptr [RSP+0x208],1`); the patch flips that immediate to `0` too. No
change alters the file length.

The three builders are `FUN_140853a80` (S3), `FUN_1461c3cc0` (Kinesis) and
`FUN_1461c5220` (STS). The tool finds each by the bytes around its constructor
call, so a build change that keeps the shape still matches; a build change that
breaks it makes the tool stop rather than patch the wrong place.

With `verifySSL` off, the WinHTTP request sets `WINHTTP_OPTION_SECURITY_FLAGS`
to `0x1100` (ignore unknown CA and CN mismatch; the date check stays on), so the
weakened check reaches only the launcher on loopback. The launcher proxy keeps
the real check: it opens its own TLS to the server and pins the server's mirror
certificate (`bins/launcher/src/aws_proxy.rs`, the same rule as `check_mirror`),
and it accepts a `CONNECT` only for the three mirror host kinds.

Run it with `--aws-proxy`:

```bash
scripts/patch-client.py "<Bin64>" --aws-proxy
scripts/patch-client.py "<Bin64>" --verify --aws-proxy
```

Reasoning, options and the safety analysis:
[docs/re/2026-09-20-windows-launcher-network.md](re/2026-09-20-windows-launcher-network.md).

## How to make the patched copy

The tool finds the function by its byte signature, not by a fixed address,
so it keeps working across game updates. Run it with the game `Bin64`
directory:

```bash
scripts/patch-client.py "/path/to/SteamLibrary/steamapps/common/New World/Bin64"
```

It writes `NewWorld.exe.orig` (a one-time backup of the official exe) and
`BlightedNewWorld.exe` (the patched copy). It never changes `NewWorld.exe`.

Check an existing copy, or revert:

```bash
scripts/patch-client.py "<Bin64>" --verify   # reports OK / WRONG, writes nothing
scripts/patch-client.py "<Bin64>" --revert   # deletes BlightedNewWorld.exe only
```

## After a game update (the important case)

When Steam updates New World, `NewWorld.exe` is replaced and the patched copy
is stale. Re-make it:

```bash
scripts/patch-client.py "<Bin64>"
```

The tool reads the new official exe, finds the signature again (new address is
fine), writes a fresh `BlightedNewWorld.exe`, and refreshes the `.orig`
backup. If the update changed the function so the signature is gone, the tool
stops with an error instead of patching the wrong place. Then re-derive the
signature (below).

## Re-deriving the signature if a build changes it

If `patch-client.py` reports "patch signature not found", the client build
changed the function. Re-find it with Ghidra (use `MAXMEM=24G` and
`-max-cpu 28`):

1. Find `SSL_CTX_set_verify`: a two-line function that writes its 2nd argument
   to `ctx+0x158` and its 3rd to `ctx+0x188`, then returns. (In build
   25810808 it is `FUN_1478f1590`.)
2. Confirm it is the game path by its caller: the gridmate DTLS context
   builder that sets the cipher string `ECDHE-RSA-AES256-GCM-SHA384` (build
   25810808: `FUN_145dcf890`).
3. Read the bytes of the `verify_mode` write (the `MOV [ctx+0x158], mode`
   instruction) and the two that follow, and update `SIGNATURE` and
   `VERIFY_MODE_WRITE` in `scripts/patch-client.py`.

If the struct offsets `0x158` / `0x188` ever change, update them in the
signature too.

For the exit patch, re-find the two `EndSession` functions: the Steam auth
manager vtable is the one its constructor stores (build 25810808:
`FUN_145a80510` stores `0x14844BA48`), slot `+0x130`; the Sonic vtable's slot
`+0x130` is the function that calls the callback first. Update
`SONIC_END_SESSION` and `STEAM_END_SESSION` in `scripts/patch-client.py`.

For the offline ticket patch, find the function that logs `Getting steam auth
session ticket.` (build 25810808: `FUN_145aa0d10`) and the handler that logs
`OnAuthSessionTicket: Authenticating using steam auth session ticket handle:
%d` (`FUN_145a94820`). Update `STEAM_GET_TICKET` and `ON_AUTH_SESSION_TICKET`
in `scripts/patch-client.py`, and `TICKET_HANDLE_FIELD` /
`TICKET_LENGTH_FIELD` if the handle (`+0x2F0`) or length (`+0x388`) moved.

For the correction patch, find the constructor writes of the eight fields of
the table above: the code that registers `g_localPlayer_CorrectionThreshold`
(`FUN_141031800`) shows the object field of each variable, and the settings
constructor writes their defaults with immediates. Update `CORRECTION_DEFAULTS`
in `scripts/patch-client.py`.

For the AWS proxy patch, re-find the `ClientConfiguration` constructor
(`FUN_147467a40`: writes the two 5,000 ms timeouts and `verifySSL = 1` into a
fresh object) and the three builders that call it with the S3, Kinesis and STS
regions. Update `CONFIG_CONSTRUCTOR`, the three `AWS_CONFIG_SITES` and the S3
`S3_VERIFY_SSL_WRITE` in `scripts/patch-client.py`. If the struct offsets change,
read them from the WinHTTP client `FUN_147491570` (it reads `proxyHost +0xA0`,
`proxyPort +0xC0`) and update `CONFIG_PROXY_HOST`, `CONFIG_PROXY_PORT` and
`CONFIG_VERIFY_SSL`.

## Launching the patched client (no EAC)

The project `launcher` runs the patched exe directly under Proton. Set the
paths in the launcher's **Options** section. The launcher saves them with its
other settings in `<user config folder>/blighted-launcher/settings.json`
(`$XDG_CONFIG_HOME` or `~/.config` on Linux, `%APPDATA%` on Windows). The
Settings section shows the exact file.

| Option | What it is | Linux | Windows |
|---|---|---|---|
| Game | `<Bin64>/BlightedNewWorld.exe` | yes | yes |
| Proton | `<Proton build>/proton`; pick a found build from the list | yes | no |
| Prefix | the launcher's own Proton prefix; empty uses `<settings folder>/prefix` | yes | no |
| Steam | the Steam install folder | yes | no |
| Override | an absolute override-config file (`-OverrideConfigPath`); optional | yes | yes |
| Proton switches | `PROTON_LOG`, `PROTON_USE_WINED3D`, `PROTON_NO_ESYNC`, `PROTON_NO_FSYNC`, `PROTON_ENABLE_NVAPI` | yes | no |
| Extra environment | `NAME=value` lines for the game process; `#` starts a comment | yes | yes |

At start the launcher finds the Steam install, its libraries
(`steamapps/libraryfolders.vdf`), the patched copy in
`steamapps/common/New World/Bin64` and the Proton builds in
`compatibilitytools.d` and `steamapps/common`. It fills an empty Game or Steam
option from what it finds. **Find paths** scans again. Each path shows a state
word: `found`, `missing` or `not set`; the prefix shows `default`, `new` or
`retail`.

The older environment variables still fill an option that is empty:
`NW_LAUNCHER_GAME_EXE`, `NW_LAUNCHER_PROTON`, `NW_LAUNCHER_STEAM_COMPAT_DATA`,
`NW_LAUNCHER_STEAM_CLIENT_INSTALL` and `NW_LAUNCHER_OVERRIDE_CONFIG`.

The prefix must be the launcher's own folder. The launcher refuses Steam's
retail prefix (`<SteamLibrary>/steamapps/compatdata/1063730`), so retail play
through Steam keeps its prefix untouched. Proton makes a new prefix in an empty
folder on the first start. Extra variables cannot replace the Steam variables
the launcher sets.

The launcher logs in as usual and starts `BlightedNewWorld.exe` through Proton
inside `bwrap`, skipping `NewWorldLauncher.exe` and so Easy Anti-Cheat. `bwrap`
binds two private files for the game process only: a hosts file that leads the
server's `[aws_mirror]` names (remote-config bucket, Kinesis, STS) to the
server, and a trust file with the server's mirror certificate added
(`docs/re/2026-09-18-remote-config-layers.md`). Without the Game, Proton and
Steam options the launcher does not start the game: the Steam launch path ran the stock client in
the retail prefix, where it called Amazon's remote-config, analytics and STS
hosts.

## Launching the patched client on Windows

Windows has no `bwrap`, so the private hosts file and trust file are not used.
Instead the client is patched with `--aws-proxy` (above) and the launcher runs a
loopback proxy on `127.0.0.1:47443` (`bins/launcher/src/aws_proxy.rs`). The
patched client sends its three AWS calls there; the proxy checks the server's
mirror certificate and relays them to the server.

The launcher starts the patched exe directly (no Proton, no `bwrap`). It needs
only the **Game** option: the path of `<Bin64>\BlightedNewWorld.exe`. The
launcher finds it in the Steam libraries when the copy exists. The Proton
options are not shown on Windows.

Steam must be running with a user logged in, as on Linux (it may be offline:
see the offline Steam ticket change). The launcher sets
`SteamAppId` and `SteamGameId` to `1063730` for the game process and refuses any
exe whose file name is not `BlightedNewWorld.exe`, so it never starts the retail
client.

### Windows live test

On a Windows PC with Steam and New World installed, and the project launcher
built (`cargo build -p launcher --release`):

1. Make the patched copy with the proxy patch, then check it:

   ```bat
   python scripts\patch-client.py "<Bin64>" --aws-proxy
   python scripts\patch-client.py "<Bin64>" --verify --aws-proxy
   ```

   Expect `patched copy: OK`. This writes `NewWorld.exe.orig` and
   `BlightedNewWorld.exe`; it never changes `NewWorld.exe`.
2. Start Steam and log in.
3. Start `launcher.exe`. In **Options**, check that **Game** shows `found`
   for `<Bin64>\BlightedNewWorld.exe`. If not, type the path, or press
   **Find paths**.

4. Log in to the community server in the launcher and press play.
5. **What passes:** the game reaches character select with the server's
   remote-config layers (the same client-read values as on Linux: UI flags and
   feature toggles). Check the launcher has the proxy port `47443` open
   (`netstat -ano | findstr 47443` shows it listening on `127.0.0.1`).
6. **If it fails:**
   - The game asks the real Amazon bucket (the config layers are missing): the
     client was not patched with `--aws-proxy`, or the proxy did not start. Run
     `--verify --aws-proxy` and check port `47443` is listening.
   - The game cannot connect at all: Steam is not running, or the server address
     or mirror certificate is wrong (the launcher refuses a mirror certificate
     that is not a non-CA certificate valid for exactly the three names).
   - A TLS error toward the proxy: the `--aws-proxy` patch did not clear
     `verifySSL`; re-run the patch and `--verify --aws-proxy`.
   - The retail `NewWorld.exe` starts, or anti-cheat runs: the **Game** option
     does not point at `BlightedNewWorld.exe`.

## Files

- `scripts/patch-client.py`: makes/checks/reverts the patched copy (with
  `--aws-proxy` for Windows).
- `scripts/patch-client.test.sh`: tests the tool on a synthetic exe
  (`just test-scripts`).
- `bins/launcher/src/aws_proxy.rs`: the Windows loopback proxy.
- `bins/launcher/testdata/proxy-tls.crt` / `proxy-tls-key.pem`: the proxy's own
  game-facing certificate (the patched client does not check it).
- `bins/launcher/testdata/proxy-mirror.crt` / `proxy-mirror-key.pem`: a stand-in
  server mirror certificate for the proxy tests.
- `<Bin64>/NewWorld.exe`: official client, never modified.
- `<Bin64>/NewWorld.exe.orig`: one-time backup of the official client.
- `<Bin64>/BlightedNewWorld.exe`: the patched client our launcher runs.
