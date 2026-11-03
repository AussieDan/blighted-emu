# Blighted client patch (BlightedNewWorld.exe)

This is the procedure that makes our patched client. Keep it current. The stock client cannot
enter the emulator without it.

## What and why

The real New World client trusts only a certificate that is built into `NewWorld.exe`, and we
do not have its private key. So the client refuses the self-signed certificate of the emulator
gateway, and world entry fails with `@mm_csdkerr_transport_security_error`. A config setting
cannot select the no-certificate transport of the client either, because the retail client
reads no external override.

The fix is one small change to the client: it stops the check of the server certificate. Then
its normal `gridmate-udp` DTLS connection accepts the emulator gateway, and the full UDP/DTLS
path stays in use.

We never change the official `NewWorld.exe`. We make a separate patched copy,
`BlightedNewWorld.exe`, next to it:

- Retail play through Steam uses the unchanged `NewWorld.exe`.
- Our launcher runs `BlightedNewWorld.exe` directly. `NewWorldLauncher.exe` starts Easy
  Anti-Cheat, so Easy Anti-Cheat never runs.

The copy has four changes. A fifth change is added with `--aws-proxy`. No change alters the
file length.

## First change: certificate bypass

The change is in one function, OpenSSL `SSL_CTX_set_verify(ctx, mode, callback)`:

```text
MOV dword ptr [RCX + 0x158], EDX    89 91 58 01 00 00   ; ctx->verify_mode  = mode
MOV qword ptr [RCX + 0x188], R8     4C 89 81 88 01 00 00 ; ctx->verify_callback = callback
RET                                 C3
```

The patch replaces the first instruction (the `verify_mode` write) with six `NOP` bytes:

```text
before:  89 91 58 01 00 00
after:   90 90 90 90 90 90
```

A fresh `SSL_CTX` starts with `verify_mode = 0` (`SSL_VERIFY_NONE`). When the mode is not
written, it stays 0, so the client checks no certificate. With mode 0, OpenSSL never calls the
verify callback, so the callback write can stay. The change is six bytes.

This setter is shared, so after the patch no TLS connection in the client checks the server
certificate. That is acceptable. The control plane (`edge-http`) presents a certificate that
the client already trusts, so a weaker check changes nothing there. On the game-server path,
the weaker check is what we want. The patch weakens only our own client when it connects to
our own server. Use only the official exe on any real or official server.

The build where this was found is Steam build `25810808`, with the patch at file offset
`0x78F0B90`. The reasoning and the reverse-engineering trail are in
[docs/re/2026-09-06-cert-bypass-patch-plan.md](re/2026-09-06-cert-bypass-patch-plan.md).

## Second change: exit to desktop

Without this change, "Exit to desktop" hangs the client. The exit path calls `EndSession` of
the Steam auth manager. That function only stores the quit callback and asks the Omni persona
client to finish later. Our setup has no persona client, so the callback never runs and the
game never quits.

`EndSession` of the Sonic auth manager runs the callback at once. The callback logs
`In callback function for IAuthManager::GetInstance()->EndSession()` and runs the console
command `quit`. The patch points the Steam `EndSession` vtable slot at the Sonic function. This
is one 8-byte pointer in `.rdata`. In build 25810808 it is at file offset `0x844AB78`, and it
changes from `0x145A8D380` to `0x145A8D310`. The Sonic function also writes 1 to the byte at
`this+0x58`. No Steam or base constructor sets that byte, and the write comes after the quit
has started.

The tool finds both functions by byte signatures. The signature of the Steam function has a
wildcard for its call offset. Then the tool finds the single pointer to the Steam function. The
reasoning and the addresses are in
[docs/re/2026-09-10-exit-to-desktop-patch.md](re/2026-09-10-exit-to-desktop-patch.md).

"Exit to main menu" is a different path. This patch does not change it.

## Third change: offline Steam ticket

Without this change, the game cannot log in while the Steam client is offline (for example,
after the PC slept and Steam lost its connection). `Game.log` stops at
`[SteamAuth] Getting steam auth session ticket.`, and the game stays at "press any button".
With Steam online, the ticket comes in about 1 s. The log then goes on with
`OnAuthSessionTicket: Authenticating using steam auth session ticket handle: ...` and
`OnAuthSessionTicket: Calling Auth stack!`.

`GetTicket` of the Steam auth manager (build 25810808: `FUN_145aa0d10`; its handler is
`FUN_145a94820`) does this after the log line:

```text
CMP  dword ptr [RBX+0x2F0], 0           ; an old ticket handle?
...  ISteamUser::CancelAuthTicket(old)  ; vtable +0x88, then clear the old ticket
...  ISteamUser::GetAuthTicketForWebApi(NULL)   ; vtable +0x70
MOV  dword ptr [RBX+0x2F0], EAX         ; the new handle
```

The ticket itself comes later in the Steam callback `GetTicketForWebApiResponse_t` (handle,
result, length, 2,560 ticket bytes). The Steam callback object at manager `+0x1A8` enters the
handler through the thunk at `0x145A9F750` (`ADD RCX,-0x1A8` / `JMP 0x145A94820`). So the
handler gets the manager in `RCX` and the response in `RDX`. The handler ignores a response
whose handle is not the stored one. For a result other than OK, it reports a Steam error. Then
it copies the ticket to `this+0x38C`, and in the `nw` backend it posts
`{"steamAuthTicket":"<hex>","steamAppId":"...","branch":"..."}` to our `/steam` route. While
the Steam client is offline, Steam never sends the callback.

Our `/steam` route (`crates/nw-login/src/game.rs`, `steam_auth`) does not read the body. The
account comes from the launcher session in the path. So the ticket has no use for us. The patch
replaces the request with a direct call of the handler. The request is 106 bytes, from the
`CMP` after the log call to the `RET`:

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

The response is in the home space of `GetTicket`. The handler saves registers only in its own
home space, below it. With length 0, the handler copies no bytes and sends the ticket `""` (the
log shows `ticket: steam|`). The game no longer asks Steam for a ticket, so the login is the
same when Steam is online or offline.

Limit: the Steam client must still run with a user logged in (online or offline). The game
calls `SteamAPI_Init` at start and shows "Steam must be running to play this game." when that
call fails. It also asks Steam for the app ID (`ISteamUtils`, local) for the `/steam` body.
This patch does not change that.

The tool finds `GetTicket` and the handler by byte signatures, with wildcards for the
RIP-relative and call offsets. Then it writes the address of the handler into the `CALL`.

## Fourth change: typed chat

The client holds typed chat back until a privacy check answers. The check is the console
variable `javelin.social.enable-vpc-restrictions` (default on). Its answer comes from a
platform service that the emulator setup does not have, so the client never sends a typed line
(live 2026-11-01: no 1291 reached the sim or `social`, while `nw-testclient chat` worked).

The settings function (build 25810808: `FUN_14616d7b0`) creates each variable as
`LEA RDX,[name]` / `LEA RCX,[RBP+var]` / `CALL` / `MOV R8B,<default>` / `LEA RDX,[RBP+var]`.
The tool finds the one creation whose `LEA RDX` points at the name of the variable and whose
default is 1. It changes the default to 0 (build 25810808: file offset `0x616E0D4`, VA
`0x14616EAD4`). The same change, at another place, turns typed chat on in another build of the
client (Inferred: the same check).

The account-age chat restriction stays on. The server answers it with `accountInProbation`.

## Fifth change: AWS proxy (`--aws-proxy`)

This change is for Windows, and for Linux when the server uses other ports than 443 and 80 (see
"Servers on other ports" below). Windows has no `bwrap` sandbox to give the game process a
private hosts file and trust file. On Linux, the hosts file cannot change a port, so only the
proxy reaches a server on other ports.

The game has four AWS SDK clients that call fixed Amazon names with certificate checks on:
remote-config (S3), analytics (Kinesis), analytics credentials (STS) and the news client (the
message of the day, the news stories, the menu tiles and their images;
`docs/re/2026-11-02-cms-news-hosts.md`). A fifth one, the region pinger, calls
`dynamodb.<region>.amazonaws.com` over plain HTTP (`docs/re/2026-11-02-region-ping.md`). Each one
builds an
`Aws::Client::ClientConfiguration` (constructor `FUN_147467a40`) and uses the WinHTTP client
(`FUN_147491570`). The factory `FUN_146b73730` picks WinINet only when the configuration byte
`+0x220` is `2`, and the constructor sets it to `0`. With `--aws-proxy`, the patch sends all
five clients through the loopback proxy of the launcher
(`docs/re/2026-09-20-windows-launcher-network.md`).

After the constructor call of each builder, the fresh config is in `RAX`. The patch replaces the
next `NOP` and the instruction after it with a jump to a code cave, and fills the bytes that
are left with `NOP`. The caves are in the first run of at least 256 INT3 padding bytes in a code
section, one 0x50-byte slot for each builder. The cave sets these fields through `RAX`:

```text
proxyHost  +0xA0  = "127.0.0.1"   (MSVC std::string: inline buffer, size 9 at +0xB0)
proxyPort  +0xC0  = 47443         (AWS_PROXY_PORT; bins/launcher/src/aws_proxy.rs uses the same)
verifySSL  +0x1B8 = 0
```

Then the cave runs the instruction that the jump replaced and jumps back. The remote-config
builder also writes `verifySSL = 1` itself a little later (`MOV byte ptr [RSP+0x208],1`). The
patch changes that immediate to `0` too.

The five builders are `FUN_140853a80` (S3), `FUN_1461c3cc0` (Kinesis), `FUN_1461c5220` (STS),
`FUN_1430089b0` (news) and `FUN_146402310` (region ping). The tool finds each one by the bytes
around its constructor call. For the region ping, the cave replays two short instructions. A build change that
keeps that shape still matches. When a build change breaks the shape, the tool stops and does
not patch the wrong place.

With `verifySSL` off, the WinHTTP request sets `WINHTTP_OPTION_SECURITY_FLAGS` to `0x1100`
(ignore an unknown CA and a CN mismatch; the date check stays on). So the weaker check reaches
only the launcher on loopback. The launcher proxy keeps the real check. It opens its own TLS to
the server and pins the mirror certificate of the server (`bins/launcher/src/aws_proxy.rs`,
the same rule as `check_mirror`). It accepts a `CONNECT` only for the mirror host kinds
(`is_aws_mirror_host`). The region pinger uses plain HTTP, so WinHTTP sends it to the proxy as
`GET http://<host>/ping`. The proxy accepts that only for a DynamoDB host and passes it unchanged
to the ping port of the server (`public_ping_port` in `config/edge.toml`, default 80). The
tunnels go to the HTTPS port of the launcher API (the port typed after the server domain,
default 443).

A copy made with `--aws-proxy` before the news and region ping clients were added has fewer
caves.
`--verify --aws-proxy` fails on it. Run the tool with `--aws-proxy` again: it writes the copy
again from `NewWorld.exe`.

Run the tool with `--aws-proxy`:

```bash
scripts/patch-client.py "<Bin64>" --aws-proxy
scripts/patch-client.py "<Bin64>" --verify --aws-proxy
```

The reasoning, the options and the safety analysis are in
[docs/re/2026-09-20-windows-launcher-network.md](re/2026-09-20-windows-launcher-network.md).

## Own-movement correction (removed)

An earlier version of the tool also raised two own-movement correction limits of the client
(`g_localPlayer_CorrectionThreshold` and `g_localPlayer_ErrorDistanceForFullCorrection`). The
tool no longer makes that change. The client keeps its retail correction values.

For reference, these are the eight correction variables of the client. The game settings
constructor writes their defaults (build 25810808: `MOV dword ptr [RDI + field], imm32` at
`0x140FEC21A`), and `FUN_141031800` registers them:

| Field | Variable (`g_localPlayer_...`) | Default |
|---|---|---|
| `0x2F0` | `CorrectionThreshold` | 0.01 m |
| `0x2F4` | `IdleVerticalCorrectionThreshold` | 0.5 m |
| `0x2F8` | `IdleHorizontalCorrectionThreshold` | 0.3 m |
| `0x2FC` | `AddedErrorDistancePerMissedServerState` | 0.4 m |
| `0x300` | `AddedErrorDistanceMaxMissedServerStates` | 5 |
| `0x304` | `ErrorDistanceForFullCorrection` | 1.0 m |
| `0x308` | `FailedCorrectionsBeforeSnap` | 4 |
| `0x30C` | `RequiredPhysicalCorrectionFraction` | 0.5 |

The help texts of the variables say this: no correction is applied while the total correction
is less than `CorrectionThreshold`, and past `ErrorDistanceForFullCorrection` a full correction
(likely a snap) happens.

## How to make the patched copy

The tool finds each patch site by byte signatures, not by fixed addresses, so it can keep
working across game updates. Run it with the game `Bin64` directory:

```bash
scripts/patch-client.py "/path/to/SteamLibrary/steamapps/common/New World/Bin64"
```

It writes `NewWorld.exe.orig` (a one-time backup of the official exe) and
`BlightedNewWorld.exe` (the patched copy). It prints the file offset of each patch site. It
never changes `NewWorld.exe`. After it writes the copy, it checks the copy again.

To check an existing copy, or to revert:

```bash
scripts/patch-client.py "<Bin64>" --verify   # reports OK / WRONG, writes nothing
scripts/patch-client.py "<Bin64>" --revert   # deletes BlightedNewWorld.exe only
```

Use `--aws-proxy` with `--verify` when the copy has the AWS proxy change.

## After a game update

When Steam updates New World, it replaces `NewWorld.exe`, and the patched copy is then old.
Make it again:

```bash
scripts/patch-client.py "<Bin64>"
```

The tool reads the new official exe, finds the signatures again (a new address is fine), and
writes a new `BlightedNewWorld.exe`. It does not refresh the `.orig` backup. When
`NewWorld.exe.orig` already exists, the tool keeps it and prints `backup kept`. So after an
update, the backup holds the old official exe. The tool always reads `NewWorld.exe`, not the
backup.

If the update changed a function so that its signature is gone, the tool stops with an error
and does not patch the wrong place. Then find the signature again (below).

## Finding a signature again after a build change

The tool stops with `<name> signature not found; the client build may have changed` or
`<name> signature found more than once; aborting`. For the certificate bypass, the name is
`certificate-verify`. Find the function again with Ghidra (use `MAXMEM=24G` and
`-max-cpu 28`):

1. Find `SSL_CTX_set_verify`. It is a two-line function that writes its second argument to
   `ctx+0x158` and its third argument to `ctx+0x188`, then returns. (In build 25810808 it is
   `FUN_1478f1590`.)
2. Make sure that it is on the game path by its caller: the gridmate DTLS context builder that
   sets the cipher string `ECDHE-RSA-AES256-GCM-SHA384` (build 25810808: `FUN_145dcf890`).
3. Read the bytes of the `verify_mode` write (the `MOV [ctx+0x158], mode` instruction) and the
   two instructions after it. Update `SIGNATURE` and `VERIFY_MODE_WRITE` in
   `scripts/patch-client.py`.

If the struct offsets `0x158` and `0x188` change, update them in the signature too.

For the exit patch, find the two `EndSession` functions again. The Steam auth manager vtable is
the one that its constructor stores (build 25810808: `FUN_145a80510` stores `0x14844BA48`),
slot `+0x130`. In the Sonic vtable, slot `+0x130` is the function that calls the callback
first. Update `SONIC_END_SESSION` and `STEAM_END_SESSION` in `scripts/patch-client.py`.

For the offline ticket patch, find the function that logs
`Getting steam auth session ticket.` (build 25810808: `FUN_145aa0d10`) and the handler that
logs `OnAuthSessionTicket: Authenticating using steam auth session ticket handle: %d`
(`FUN_145a94820`). Update `STEAM_GET_TICKET` and `ON_AUTH_SESSION_TICKET` in
`scripts/patch-client.py`. If the handle (`+0x2F0`) or the length (`+0x388`) moved, also update
`TICKET_HANDLE_FIELD` and `TICKET_LENGTH_FIELD`.

For the typed chat patch, find the settings function that creates
`javelin.social.enable-vpc-restrictions` (build 25810808: `FUN_14616d7b0`). If the shape of a
variable creation changed, update `VARIABLE_CREATION` in `scripts/patch-client.py`.

For the AWS proxy patch, find the `ClientConfiguration` constructor again (`FUN_147467a40`: it
writes the two 5,000 ms timeouts and `verifySSL = 1` into a fresh object). Also find the three
builders that call it with the S3, Kinesis and STS regions, the news fetch that logs
`Loading CMS HTTP Request URL: %s`, and the region pinger's builder (called by the set-up that
names `AwsRegionPinger`). Update `CONFIG_CONSTRUCTOR`, the five `AWS_CONFIG_SITES` and
`S3_VERIFY_SSL_WRITE` in `scripts/patch-client.py`. If the struct
offsets change, read them from the WinHTTP client `FUN_147491570` (it reads `proxyHost +0xA0`
and `proxyPort +0xC0`). Then update `CONFIG_PROXY_HOST`, `CONFIG_PROXY_HOST_SIZE`,
`CONFIG_PROXY_PORT` and `CONFIG_VERIFY_SSL`.

## Launching the patched client (no EAC)

The project `launcher` runs the patched exe directly under Proton. Set the paths in the Options
section of the launcher. The launcher saves them with its other settings in
`<user config folder>/blighted-launcher/settings.json` (`$XDG_CONFIG_HOME` or `~/.config` on
Linux, `%APPDATA%` on Windows). The Settings section shows the exact file.

| Option | What it is | Linux | Windows |
|---|---|---|---|
| Game | `<Bin64>/BlightedNewWorld.exe` | yes | yes |
| Proton | `<Proton build>/proton`; pick a found build from the list | yes | no |
| Prefix | the launcher's own Proton prefix; empty uses `<settings folder>/prefix` | yes | no |
| Steam | the Steam install folder | yes | no |
| Override | an absolute override-config file (`-OverrideConfigPath`); optional | yes | yes |
| Proton switches | `PROTON_LOG`, `PROTON_USE_WINED3D`, `PROTON_NO_ESYNC`, `PROTON_NO_FSYNC`, `PROTON_ENABLE_NVAPI` | yes | no |
| Extra environment | `NAME=value` lines for the game process; `#` starts a comment | yes | yes |

At start, the launcher finds the Steam install, its libraries
(`steamapps/libraryfolders.vdf`), the patched copy in `steamapps/common/New World/Bin64`, and
the Proton builds in `compatibilitytools.d` and `steamapps/common`. It fills an empty Game or
Steam option from what it finds. The Find paths button scans again. Each path shows a state
word: `found`, `missing` or `not set`. The prefix shows `default`, `found`, `new` or `retail`.

The older environment variables still fill an option that is empty: `NW_LAUNCHER_GAME_EXE`,
`NW_LAUNCHER_PROTON`, `NW_LAUNCHER_STEAM_COMPAT_DATA`, `NW_LAUNCHER_STEAM_CLIENT_INSTALL` and
`NW_LAUNCHER_OVERRIDE_CONFIG`.

The prefix must be the launcher's own folder. The launcher refuses the retail prefix of Steam
(`<SteamLibrary>/steamapps/compatdata/1063730`), so retail play through Steam keeps its prefix
unchanged. Proton makes a new prefix in an empty folder on the first start. Extra variables
cannot replace the Steam variables that the launcher sets.

The launcher logs in as usual and starts `BlightedNewWorld.exe` through Proton inside `bwrap`.
It skips `NewWorldLauncher.exe`, and so it skips Easy Anti-Cheat. `bwrap` binds two private
files for the game process only. One is a hosts file that sends the `[aws_mirror]` names of the
server (remote-config bucket, Kinesis, STS, the region ping host and the two news hosts) to the
server. The other is a trust file with the
mirror certificate of the server added (`docs/re/2026-09-18-remote-config-layers.md`). Without
the Game, Proton and Steam options, the launcher does not start the game. The Steam launch path
ran the stock client in the retail prefix, where it called the remote-config, analytics and STS
hosts of Amazon.

## Servers on other ports

`config/edge.toml` sets the ports that players reach the server on: `public_https_port` (default
443) and `public_ping_port` (default 80). The listeners (`https_listen`, `ping_listen`) can use
other ports than these when a router or proxy forwards the public ports to them.

When `public_https_port` is not 443:

1. Players type the port after the domain in the launcher, for example `example.org:8443`.
2. The server puts the port in its links to `login.<domain>` and `game.<domain>` (the
   `-ChannelService` argument, the channel document, the remote-config service endpoints and the
   Discord callback). For Discord login, register the callback with the port in the Discord
   Developer Portal.
3. The launcher sends the Amazon names of the game to that port through its loopback proxy.

When `public_ping_port` is not 80, the launcher's proxy sends the region ping to it.

The launcher runs the loopback proxy on Windows and on Linux. With other ports, the client must
be patched with `--aws-proxy` on Linux too. An unpatched client on Linux uses the private hosts
file, which leads to ports 443 and 80 only.

Unknown: the game client reads a link with a port (`game.example.org:8443/s/<session>`). The
client already takes a port in `-GatewayAddr`, so it probably does. It is not live-checked yet.

## Launching the patched client on Windows

Windows has no `bwrap`, so the private hosts file and trust file are not used. Instead, the
client is patched with `--aws-proxy` (above), and the launcher runs a loopback proxy on
`127.0.0.1:47443` (`bins/launcher/src/aws_proxy.rs`). The patched client sends the calls of its
five AWS SDK clients there. The proxy checks the mirror certificate of the server and relays the calls to the
server.

The launcher starts the patched exe directly (no Proton, no `bwrap`). It needs only the Game
option: the path of `<Bin64>\BlightedNewWorld.exe`. When the copy exists, the launcher finds it
in the Steam libraries. Windows does not show the Proton options.

Steam must run with a user logged in, as on Linux. Steam can be offline (see the offline Steam
ticket change). The launcher sets `SteamAppId` and `SteamGameId` to `1063730` for the game
process. It refuses any exe whose file name is not `BlightedNewWorld.exe`, so it never starts
the retail client.

### Windows live test

You need a Windows PC with Steam and New World installed, and the project launcher built
(`cargo build -p launcher --release`).

1. Make the patched copy with the proxy patch, then check it:

   ```bat
   python scripts\patch-client.py "<Bin64>" --aws-proxy
   python scripts\patch-client.py "<Bin64>" --verify --aws-proxy
   ```

   Expect `patched copy: OK`. This writes `NewWorld.exe.orig` and `BlightedNewWorld.exe`. It
   never changes `NewWorld.exe`.
2. Start Steam and log in.
3. Start `launcher.exe`. In Options, make sure that Game shows `found` for
   `<Bin64>\BlightedNewWorld.exe`. If it does not, type the path or press Find paths.
4. Log in to the community server in the launcher and press play.
5. Pass: the game gets to character select with the remote-config layers of the server (the
   same client-read values as on Linux: UI flags and feature toggles). Make sure that the
   launcher has the proxy port `47443` open (`netstat -ano | findstr 47443` shows it listening
   on `127.0.0.1`).
6. Fail:
   - The game asks the real Amazon bucket (the config layers are missing). The client was not
     patched with `--aws-proxy`, or the proxy did not start. Run `--verify --aws-proxy` and
     make sure that port `47443` is listening.
   - The game cannot connect at all. Steam is not running, or the server address or the mirror
     certificate is wrong. The launcher refuses a mirror that is not exactly one certificate,
     that is a CA, that is not valid now for each mirror name, or that has a name that is not
     a remote-config, Kinesis, STS, region ping, news or news image host.
   - A TLS error toward the proxy. The `--aws-proxy` patch did not clear `verifySSL`. Run the
     patch and `--verify --aws-proxy` again.
   - The retail `NewWorld.exe` starts, or anti-cheat runs. The Game option does not point at
     `BlightedNewWorld.exe`.

## Files

- `scripts/patch-client.py`: makes, checks and reverts the patched copy (with `--aws-proxy`
  for Windows).
- `scripts/patch-client.test.sh`: tests the tool on a synthetic exe (`just test-scripts`).
- `bins/launcher/src/aws_proxy.rs`: the loopback proxy (Windows, and Linux with a client patched
  with `--aws-proxy`).
- `bins/launcher/testdata/proxy-tls.crt` and `proxy-tls-key.pem`: the proxy's own
  certificate toward the game (the patched client does not check it).
- `bins/launcher/testdata/proxy-mirror.crt` and `proxy-mirror-key.pem`: a stand-in mirror
  certificate of the server for the proxy tests.
- `<Bin64>/NewWorld.exe`: the official client, never changed.
- `<Bin64>/NewWorld.exe.orig`: the one-time backup of the official client.
- `<Bin64>/BlightedNewWorld.exe`: the patched client that our launcher runs.
