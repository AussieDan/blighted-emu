# Client protocol: login, GridMate, and world entry

Date: 2026-08-29
Build: Steam build `22469132`, `Bin64/NewWorld.exe`, image base `0x140000000`, x86-64, Windows compiler spec. Ghidra project `<re-scratch>/ghidra` (program `NewWorld.exe`, 355,815 functions).
Method: static analysis of the already-analyzed project, opened read-only with analysis left off (`analyzeHeadless -process NewWorld.exe -noanalysis -readOnly`). The game was not run. No game file was changed. Easy Anti-Cheat was not analyzed. Static analysis did not observe execution.

Labels: **Observed** = an address, a string, or decompiler output in this build. **Inferred** = a conclusion from that evidence. **Unknown** = not established.

Related notes: [2026-08-29 client install findings](2026-08-29-client-install-findings.md), [2026-08-31 endpoint configuration](2026-08-31-client-endpoint-configuration.md) (RE task 1). This note is RE tasks 2 to 5 in the architecture spec, section 11.3.

Where this note and `docs/research/02-network-protocol-and-login.md` disagree, the binary wins. Those disagreements are marked below.

REA `open_binary` was not used. That Ghidra provider always imports the executable into a new project and runs auto-analysis. This pass reused the existing project and did not re-analyze it.

## 1. Login

### 1.1 After the channel config

**Observed.** `FUN_146450250` (`0x146450250`) reads the channel JSON. It logs `Channel name: %s` from `channelName`, walks `platformMetadata`, and for each cluster reads `displayName`. Public API objects are selected by `clientTag`:

| `clientTag` | Fields read | URL the client builds |
|---|---|---|
| `authStack` | `apiEndpoint`, `awsRegion` | `https://%s/Prod` |
| `loginGateway` | `apiEndpoint`, `awsRegion` | the endpoint string as stored |
| `JavelinGatewayServiceV2` | `apiEndpoint` | `https://%s/prod` |
| `JavelinGatewayService-CF` | endpoint host | logged as `Gateway Stack` / `Http Gateway Stack` |

`FUN_1464211f0` (`0x1464211f0`), called from `0x146450250`, reads `omniTokenUrl`. The same parser also accepts `nwTokenUrl` and `personaHostV2` (see the endpoint note).

**Observed.** These path strings sit in one block at file region `0x84ff388` (VA `0x148500b88` and the following strings). Operation names in the same region have a pointer table at `0x1484fa070`:

| Operation name | Path string in the same block |
|---|---|
| `GetLoginInfo` | `/game/getlogininfo`, `/game/getlogininfo/jwt`, `/game/getlogininfo/jwt/omni` |
| `ListWorlds` | `/game/worlds/list`, `/game/worlds/list/jwt`, `/game/worlds/list/jwt/omni` |
| `ValidateCharacter` | `/game/worlds/%s/characters/validator`, `.../jwt`, `.../jwt/omni` |
| `PostGameWorldsWorldIdCharacters` (string only) | `/game/worlds/%s/characters`, `.../jwt`, `.../jwt/omni` |
| `QueueLogin` / `PostGameLoginQueueV2` | `/game/login/queue/v2`, `.../jwt`, `.../jwt/omni` |
| `RefreshQueuedLogin` / `PostGameLoginQueueV2TicketId` | `/game/login/queue/v2/%s`, `.../%s/jwt/omni` |
| `LinkIdentity` / `UnlinkIdentity` | `/game/linkIdentity`, `/game/unlinkIdentity` |

**Observed.** The selector that chooses between the bare path, `.../jwt`, and `.../jwt/omni` is in the request builders (`FUN_146417490` at `0x146417490` for `QueueLogin`, and `FUN_146417a70` at `0x146417a70` for `RefreshQueuedLogin`) together with `FUN_14646fca0` (`0x14646fca0`):

1. `FUN_14646fca0` checks that `FUN_146438f60` (`0x146438f60`) returns a non-empty auth token string and that the gateway endpoint string at `param_1 + 0xa0` contains `.cloudfront.net`.
2. When `FUN_14646fca0` returns `0`, `FUN_146417490` selects `/game/login/queue/v2` and `FUN_146417a70` selects `/game/login/queue/v2/%s`.
3. When `FUN_14646fca0` returns `1`, the caller checks the auth manager (`FUN_145a8dce0`, `0x145a8dce0`). If virtual slot `+0xf0` returns `2` or virtual slot `+0xe8` returns `4`, it selects `/game/login/queue/v2/jwt/omni` (and `/game/login/queue/v2/%s/jwt/omni`). Otherwise it selects `/game/login/queue/v2/jwt` (and `/game/login/queue/v2/%s`).
4. The HTTP header `x-nw-auth` (`0x148500b88`) is populated from `FUN_146438f60` by `FUN_1464559a0` (`0x1464559a0`) and written into request header maps by `FUN_1474ebca0` (`0x1474ebca0`), `FUN_1474eba80` (`0x1474eba80`), `FUN_1474eb860` (`0x1474eb860`), `FUN_1474eb640` (`0x1474eb640`), `FUN_1474eb420` (`0x1474eb420`), and `FUN_1474eb200` (`0x1474eb200`).

**Observed.** Connection-state names, pointer table `0x1484f9ff0`, used by `FUN_14644a070` (`0x14644a070`):

`Disconnected`, `QueryGameUpdateCheck`, `WaitingForGameUpdateCheck`, `QueueGameLogin`, `WaitingForQueuedLogin`, `QueryForRemoteConfigClass`, `WaitingForRemoteConfigClass`, `ObtainREPRequirements`, `WaitingForREPRequirements`, `StartREPConnection`, `WaitingForREPConnection`, `WaitingForActorGameConnection`, `WaitingForSpawnPoint`, `WaitingForPlayerSpawn`, `InGame`.

`StartREPConnection` logs `GameConnectionWrapper: start REP connection RepAddress = %s, worldId = %s`.

### 1.2 Steam ticket and token

**Observed.** `FUN_145a9fbe0` (`0x145a9fbe0`) logs `Getting steam auth session ticket.`, calls `SteamInternal_ContextInit`, then a virtual method at offset `0x70`. It stores the returned 32-bit handle at object `+0x2f0` and clears `0xa00` bytes at `+0x38c`. If a previous handle is non-zero it calls virtual offset `0x88` first. **Inferred:** these are the Steam user-interface ticket calls. The interface slot names are not in the binary.

**Observed.** `FUN_145a936f0` (`0x145a936f0`) is the `OnAuthSessionTicket` callback. It logs the handle and `ticket: %s, ticketNoPrefix: 0x%s`. It reads `client-connection.client-gateway.signing-host`, `.endpoint`, `.http-endpoint`, and `.region`. It appends the 6-byte string `/steam` (`0x14844bbb8`) and issues an HTTP call with arguments `10000`, `3000`, and `5` (`FUN_141722280`). Failure strings include `@mm_http_NoResponse`, `No auth stack endpoint!`, and `Persona failed to initialize!` (`@mm_authresult_Error_Persona`).

**Observed.** Omni token request builder `FUN_1479c9a90` (`0x1479c9a90`), source path `OmniSDK\sdk\source\gateway\ags_fed_acc_token_serv_gateway.cpp`. The JSON object is:

- `platformIdentityType` (string from `FUN_1479c23e0`, `0x1479c23e0`)
- `platformAuth` (object). Enum value `1` writes `ticket`. Values `3`, `5`, and `10` write `authCode`. Values `6`, `7`, `8`, `11`, and `12` write `token`, or `nsaToken` plus `naToken`. Value `100` writes `platformAccountId` and then `token`. Values `2`, `4`, `9`, and `13` write `token`.
- `fallbackToken`
- `agsAccountAuthToken` when a fourth argument is non-null
- `attributionData`

**Observed.** `FUN_1479c23e0` (`0x1479c23e0`) looks up the integer platform enum in hash table `DAT_14a829178` (returning `none` from `0x147fdbab0` when absent). Static initializer `FUN_14028fdb0` (`0x14028fdb0`) populates `DAT_14a829178` with all 14 values:

| Enum value | `platformIdentityType` string | Address |
|---|---|---|
| `1` | `steam` | `0x147f48c88` |
| `2` | `xbox-live` | `0x1495d9550` |
| `3` | `psn` | `0x1495d9580` |
| `4` | `xbox-live-cert` | `0x1495d9560` |
| `5` | `psn-cert` | `0x1495d9588` |
| `6` | `nsa` | `0x1495d95a0` |
| `7` | `nsa-cert` | `0x1495d95b0` |
| `8` | `nsa-cert-dev` | `0x1495d95c0` |
| `9` | `xbox-live-dev` | `0x1495d9570` |
| `10` | `psn-dev` | `0x1495d9598` |
| `11` | `nsa-dev` | `0x1495d95a8` |
| `12` | `nsa-compat` | `0x1495d95d0` |
| `13` | `luna` | `0x1495d95dc` |
| `100` | `mock` | `0x147fc8184` |

**Observed.** Token response handler `FUN_1479c9f00` (`0x1479c9f00`, `GetOrCreateToken`) parses the HTTP reply body as JSON and calls `FUN_1479b6d00` (`0x1479b6d00`). `FUN_1479b6d00` reads `limitedUseToken`, `fallbackToken`, `platformAccount`, `account`, `isNewAccount`, `conflictingAccount`, `accessToken`, `expiresIn`, and `region`. Error strings it handles: `INVALID_FALLBACK_TOKEN`, `INVALID_AGS_AUTH_TOKEN`. `FUN_14028ed40` (`0x14028ed40`) references `PLAYER_IS_BANNED` and `INVALID_PLATFORM_AUTH`. `FUN_1479b6d00` checks that the required keys (`accessToken`, `fallbackToken`, `platformAccount` at `0x1495d7bf8`) exist in the JSON object, reads `accessToken` as an opaque string (`FUN_1479d6770`, `0x1479d6770`), reads `expiresIn` as integer seconds (multiplying by `1000`), and moves the strings into the token record (`FUN_1479b2ff0`, `0x1479b2ff0`). The client does not split, base64-decode, or verify a JWT signature on `accessToken`. The strings `jwks.json` and `openid-configuration` are absent.

### 1.3 Login-queue reply

**Observed.** Three functions, all optional-field readers (a key is stored only when `FUN_147465730` says it is present):

1. `FUN_1474e89d0` (`0x1474e89d0`) looks up the object `LoginQueueResponse` and passes it to (2).
2. `FUN_1474e4f20` (`0x1474e4f20`) reads the queue object and, if `Token` is present, passes that object to (3).
3. `FUN_1474e5990` (`0x1474e5990`) reads the token object.

JSON shape:

```json
{
  "LoginQueueResponse": {
    "AllowQueueTransfer": true,
    "EstimatedTime": 0,
    "Position": 0,
    "QueueName": "",
    "RecommendedTransferWorldId": "",
    "RefreshInterval": 0,
    "TicketId": "",
    "Token": {
      "AccountAge": 0,
      "AccountIsLocked": false,
      "ChannelId": "",
      "CharacterId": "",
      "ClientCapabilities": "",
      "GenerateTime": 0.0,
      "HostHash": "",
      "IsPermanentAppOwner": false,
      "IsTrialOwner": false,
      "IssueTime": 0.0,
      "JwtClaims": "",
      "LocationGroupId": "",
      "LocationId": "",
      "PersonaId": "",
      "RepAddress": "",
      "Signature": "",
      "SteamAppId": 0,
      "SteamUserId": "",
      "TicketId": "",
      "TokenVersion": 0,
      "WorldId": ""
    }
  }
}
```

Getters:
- `FUN_147465410` (`0x147465410`) is the boolean reader (`AllowQueueTransfer`, `AccountIsLocked`, `IsPermanentAppOwner`, `IsTrialOwner`).
- `FUN_147465490` (`0x147465490`) is the integer reader (`EstimatedTime`, `Position`, `RefreshInterval`, `AccountAge`, `SteamAppId`, `TokenVersion`).
- `FUN_147465440` (`0x147465440`) is the floating-point (`double`) reader (`GenerateTime` and `IssueTime`, returning `*(double *)(cjson + 0x30)`).
- `FUN_1474654e0` (`0x1474654e0`) is the string reader (every other field above, including outer `TicketId` at `0x644974656b636954`, `HostHash` at `0x6873614874736f48`, `JwtClaims` at `0x148703bc0`, `RepAddress`, `Signature`, and inner `TicketId`).

`RepAddress` and `Signature` are strings inside `Token`. They are not fields of the outer `LoginQueueResponse` object. Research file 02 places them on the outer object. The binary disagrees.

**Observed.** The request writer `FUN_1474ecc80` (`0x1474ecc80`) emits an object `LoginQueueRequest` when a presence flag at `+0x250` is set. `FUN_1474ec7b0` (`0x1474ec7b0`) writes, each behind its own presence flag:

| Field | JSON kind |
|---|---|
| `CharacterId` | string |
| `ClientCapabilities` | array of strings |
| `IsTrialOwner` | boolean |
| `SteamAppId` | number |
| `SteamAuthTicket` | string |
| `WorldId` | string |

**Observed.** Ticket log strings, not the community text `No login ticket received` (that text is absent):

- `Received a login ticket: %s, location: %s, %s` in `FUN_14643d110` (`0x14643d110`)
- `RefreshLoginQueueTicket failed: no login ticket.`
- `LoginQueueTicket failed: no login ticket.` (no code reference recorded)
- `Waiting in login queue... Ticket (%s), Position (%d)`
- `Queuing login for world (%s) and character (%s)`

**Observed.** How the client consumes `LoginQueueResponse` and `Token`:

1. `FUN_1464264f0` (`0x1464264f0`) converts the parsed `LoginQueueResponse` (`param_2`) into the internal queue result struct (`param_3`). It copies outer `TicketId` (`param_2 + 0x70`) to `param_3 + 0x00`, `Position` (`param_2 + 0x20`) to `param_3 + 0x20`, `EstimatedTime` (`param_2 + 0x08`) to `param_3 + 0x24`, `RefreshInterval` (`param_2 + 0x64`) to `param_3 + 0x28`, `QueueName` (`param_2 + 0x24`) to `param_3 + 0x40`, `RecommendedTransferWorldId` to `param_3 + 0x280`, and `AllowQueueTransfer` to `param_3 + 0x2a0`.
2. If the presence flag for `Token` (`*(longlong *)(param_2 + 0x238)`) is non-zero, `FUN_1464264f0` calls `FUN_1464269d0` (`0x1464269d0`) -> `FUN_1464040c0` (`0x1464040c0`) -> `FUN_146403a80` (`0x146403a80`) to build the `0x208`-byte internal `Token` struct at `param_3 + 0x70` and sets `*(char *)(param_3 + 0x278) = 1`. In `FUN_1464269d0`, missing optional strings default to `""`, missing integers default to `0` (`GenerateTime` and `IssueTime` are truncated with `(int)*(double *)`), and `AccountIsLocked` (`param_1 + 0x1a0`, presence byte at `+0x1a1`) retains its presence flag.
3. `FUN_14643d110` (`0x14643d110`) receives `param_3`:
   - If `param_3 == 0` (the HTTP/gateway call failed), it logs `RefreshLoginQueueTicket failed: no login ticket.` and disconnects.
   - Whenever `param_3 != 0`, it logs `Received a login ticket: %s, location: %s, %s` using outer `TicketId` (`param_3 + 0x00`) and `QueueName` (`param_3 + 0x40`, mapped to `@mm_loginservices_location_<QueueName>`).
   - If `*(char *)(param_3 + 0x278) == 0` (no `Token` object in the reply), the client stays in the login queue, logs `Waiting in login queue... Ticket (%s), Position (%d)`, and schedules a refresh timer at `param_1 + 0x1358` using `RefreshInterval`.
   - If `*(char *)(param_3 + 0x278) != 0` (`Token` is present), `FUN_14643d110` compares the first string field of the stored token (`param_1 + 0x10e0`, `Signature`) with `param_3 + 0x70` (`Signature`) via `FUN_14114b200` (`0x14114b200`, `AZStd::basic_string::operator!=`). Because `param_1 + 0x10e0` is initialized to empty (`""`), a non-empty `Signature` in `Token` is required for `FUN_14114b200` to return `true` and invoke `FUN_146468370(param_1, param_3 + 0x70)` (`0x146468370`).
   - `FUN_146468370` copies the `0x208`-byte `Token` struct into `param_1 + 0x10e0` (`FUN_146411ae0`, `0x146411ae0`) and transitions connection state to `7` (`ObtainREPRequirements`, when `client-connection.enable-remoteCfg` is `false`) or `5` (`QueryForRemoteConfigClass`, when `true`).
   - The client does not perform cryptographic signature verification on `Token.Signature`; it stores the `Token` struct at `GameConnection + 0x10e0` and later serializes it into `RegistrationRequestV3Msg + 0xc0` (and `RegistrationRequestV2Msg + 0xc0`) to send to the REP server.

**Observed.** How `RepAddress` is parsed and split into host and port:

1. In `FUN_14644a070` (`0x14644a070`) state `9` (`StartREPConnection`), `FUN_146425f20` (`0x146425f20`) passes `RepAddress` (`param_1 + 0x1100`, offset `+0x20` inside the stored `Token` struct at `param_1 + 0x10e0`) to virtual slot `+0x18` (`FUN_146b6d600`, `0x146b6d600`) on the REP connection object (`*(param_1 + 0x1000)`).
2. `FUN_146b6d600` calls `FUN_140870880` (`0x140870880`) -> `FUN_140878e80` (`0x140878e80`) to parse `RepAddress` into an `0x48`-byte endpoint struct (`hostname` string at `+0x00`, `ip` string at `+0x20`, `uint16` port at `+0x40`):
   - `FUN_140878e80` searches `RepAddress` for `':'` (`0x3a`) with `memchr`.
   - The substring before `':'` (or the entire string when `':'` is absent) is tested with `WS2_32` ordinal 11 (`inet_addr`). If `inet_addr` returns `-1` (`INADDR_NONE`), the substring is stored as a hostname at `+0x00`; otherwise it is stored as an IPv4 address string at `+0x20`.
   - When `':'` is present, the substring after `':'` is converted with `atoi`. If `1 <= port <= 65535` (`iVar1 - 1U < 0xffff`), `(uint16)port` is stored at `+0x40`.
3. `FUN_146b6d600` then checks `FUN_140874f50` (`0x140874f50`, returns `*(uint16 *)(endpoint + 0x40)`). If the port is `0`, it reads `client-connection.transport-options.rep-default-port` (default `0x829b` = `33435` in `FUN_146168bc0`, `0x146168bc0`) and stores that port at `+0x40` via `FUN_140879250` (`0x140879250`).
4. `FUN_146b6d600` validates the endpoint with `FUN_140876c50` (`0x140876c50`), which requires `(hostname_len != 0 || ip_len != 0) && port != 0`. If `RepAddress` is empty, `FUN_140876c50` returns `0` and `FUN_146b6d600` fails with connection error `4` (`FUN_146b6c500`, `0x146b6c500`).
5. Separately, inside the GridMate transport wrapper (`FUN_146b39730`, `0x146b39730`), peer address strings returned by the carrier driver (`vtable + 0x38`) use `<host>|<port>` format and are split on `'|'` (`0x7c`) with `memchr` and `strtol(..., 10)` into `FUN_1408708d0` (`0x1408708d0`).

### 1.4 Certificate trust

**Observed.** A PEM block in the executable (file `0x858ec60`) decodes as one self-signed X.509 certificate. Subject and issuer are `C=US, ST=California, L=SNA11, O=Amazon, OU=Amazon Game Studios, CN=New World`. `notBefore` is 2025-08-04 23:15:36 GMT. `notAfter` is 2027-01-06 23:15:36 GMT. No `PRIVATE KEY` block is next to it. The PEM body is not copied here. The endpoint note records the same certificate and that no launch argument or config key replaces it.

**Observed.** `FUN_146b6a270` (`0x146b6a270`) reads `client-connection.transport-options.transport-type`. For the exact strings `gridmate-udp` and `gridmate-udp-bsd` it constructs the transport with `FUN_146b34780`, and it passes the embedded PEM (`0x149f80d90`). Any other transport string uses `FUN_146b3de50` and does not receive that PEM.

**Observed.** `FUN_145dce750` (`0x145dce750`) builds the secure context:

- It sets the cipher list to the literal `ECDHE-RSA-AES256-GCM-SHA384` (`FUN_1478efe40`).
- If a PEM text pointer on the driver is non-null (`+0x288`), `FUN_145dc8de0` (`0x145dc8de0`) finds each `-----BEGIN CERTIFICATE-----` … `-----END CERTIFICATE-----` span, parses it (`FUN_145dc91f0` / `FUN_14777f1e0`), and adds each parsed certificate (`FUN_14777d770`). Verify is then set with a null callback (`FUN_1478eff50`).
- The mode argument is `1` or `2`, from a flag byte at driver `+0x4e` (`0` yields `1`, non-zero yields `2`).
- If that PEM pointer is null, the same call uses `FUN_1402a1a70` (`0x1402a1a70`), which returns `1`.

**Inferred:** `FUN_1478eff50` is `SSL_CTX_set_verify`, and modes `1` and `2` are the OpenSSL peer-verify modes. The cipher string and the PEM parse are the evidence. No hostname check appears in `FUN_145dce750` or `FUN_145dc8de0`. **Observed:** `FUN_146b34780` (`0x146b34780`) stores the PEM string (`param_3`) at object `+0x80` (`param_1 + 0x10`) and sets vtable `0x14858d200` at object `+0x30`. When `FUN_146b385a0` (`0x146b385a0`) initializes the carrier, it passes the PEM buffer from `+0x80` into the carrier descriptor (`local_128`, read at driver `+0x288` by `FUN_145dce750`) and invokes virtual slot `+0x28` (`FUN_146ab5400`, `0x146ab5400`) on `object + 0x30` when `*(char *)(object + 0xfc) != 0` (`javelin.enable-lz4-compression`).

A separate string mentions `caPath` and a RETAIL / sandbox certificate choice. It has no code reference. It is not the REP trust path above.

## 2. GridMate Carrier

**Observed.**

- Log and thread names: `GridMate-Carrier` (`FUN_140f4c5a0`, `0x140f4c5a0`), `GridMate-Carrier Packet Send Thread` (`FUN_140f69c00`, `0x140f69c00`).
- Config keys in one string block: `javelin.enable-lz4-compression`, `javelin.gridmate.bind-to-specific-interface`, `javelin.gridmate.shared-recv-buffers`, `javelin.gridmate.send-warning-msgs`, `javelin.gridmate.process-resends-last`, `javelin.gridmate.warning-conn-threshold-divisor`, `javelin.gridmate.warning-no-sends-threshold-ms`, `javelin.gridmate.warning-check-period-ms`, `javelin.gridmate.client.initial-send-pool-size`, `javelin.gridmate.client.max-send-pool-size`.
- `javelin.enable-lz4-compression` is referenced from `FUN_140279760` (`0x140279760`) and from `FUN_14616c670` (`0x14616c670`, 111,309 bytes, not decompiled).
- `LZ4_compressBound` is present at `0x1479709c0` (library signature). For an input below `0x7e000000` it returns `n + (n / 0xff) + 0x10`. `FUN_146abec70` (`0x146abec70`) only calls it. Callers are data pointers (`0x148588dd8`, `0x14ac113a8`), not a decompiled send function.
- The ASCII string `LZ4` is absent. The config key uses lowercase `lz4`.
- Transport error strings: `Carrier driver error ConnectionID: %p ErrorCode: 0x%08x` (`FUN_140f6f7a0`), `Carrier security error ConnectionID: %p ErrorCode: 0x%08x` (`FUN_140f724d0`).

**Observed.** The connection object keeps four sequence groups. `FUN_140f76d60` (`0x140f76d60`) prints them:

| Log label | Connection offsets | Width |
|---|---|---|
| `conn.sendSeqNum` | `+0x6c`, `+0x6e`, `+0x70`, `+0x72` | four `uint16` |
| `conn.sendReliableSeqNum` | `+0x74`, `+0x76`, `+0x78`, `+0x7a` | four `uint16` |
| `conn.toSend` | `+0x90`, `+0xd0`, `+0x110`, `+0x150` | four pointers, stride `0x40` |
| `conn.toReceive` | `+0x1a0`, `+0x1e0`, `+0x220`, `+0x260` | four pointers, stride `0x40` |

**Inferred:** the carrier has four channels, indexes 0 through 3, and a separate reliable sequence per channel. Research file 02 names channels 0, 1, and 3. Slot 2 is present in this object. Which slots are used on the wire was not shown.

**Observed.** The send thread body is `FUN_140f8b810` (`0x140f8b810`), started by `FUN_140f69c00`. It sends buffers that `FUN_140f69c00` sizes at `0x600` bytes, with a side record of `0x1c` bytes per slot. On a return code it calls `WS2_32` ordinal 111 and retries while the code is `0x2747` (10055). It does not write a mode byte, a format byte, or a sequence into the buffer.

**Observed.** LZ4 sits in one cluster. `LZ4_compressBound` is `0x1479709c0`. The pointer table at `0x148588dc0` holds:

| Slot | Function | What it calls |
|---|---|---|
| `0x148588dd8` | `0x146abec70` | `LZ4_compressBound` |
| `0x148588de0` | `0x146ab3bd0` | `0x147970930` (0x90 bytes before `LZ4_compressBound`). Returns 2 when the callee result is below 1. |
| `0x148588de8` | `0x146ab6540` | `0x1479714e0`. On success it stores the output pointer and the byte count. |

**Observed.** The three wrappers are slots in a vtable. The code takes the base `0x148588db8`. `FUN_146ab5400` (`0x146ab5400`) allocates 8 bytes and stores that base as the object's only pointer. Slot order from that base:

| Offset | Function |
|---|---|
| `+0x00` | `0x146aad330` |
| `+0x08` | `0x14029f880` |
| `+0x10` | `0x146ae0240` |
| `+0x18` | `0x140c9f3f0` |
| `+0x20` | `0x146abec70` (`LZ4_compressBound` wrapper) |
| `+0x28` | `0x146ab3bd0` |
| `+0x30` | `0x146ab6540` |
| `+0x38` | `0x141752700` |
| `+0x40` | `0x141754050` |

**Observed.** `FUN_140f4c5a0` (`0x140f4c5a0`) spawns the `GridMate-Carrier` worker thread running `FUN_140f82340` (`0x140f82340`). On each tick, `FUN_140f82340` calls:

1. `FUN_140f777f0` (`0x140f777f0`) for connection maintenance, keepalives, and handshake state (`SM_CONNECT` handshake packets).
2. `FUN_140f898e0` (`0x140f898e0`) to receive and unpack incoming datagrams.
3. Virtual slot `+0x08` on `param_1[1]` (`driver->Update()`).
4. `FUN_140f8ab70` (`0x140f8ab70`) to pack, optionally LZ4-compress, and send outgoing datagrams, followed by virtual slot `+0x18` on `param_1[1]` (`FUN_140f7fc60`, `0x140f7fc60`).

**Observed.** The 4-byte datagram header, LZ4 compression call site, and frame layout in `FUN_140f8ab70` (`0x140f8ab70`, send) and `FUN_140f898e0` (`0x140f898e0`, receive):

1. **Datagram 4-byte header (`FUN_140f8ab70` send / `FUN_140f898e0` receive)**:
   - `FUN_140f8ab70` resets the bit-packer at `param_1 + 0x5a0` (vtable `0x147fbe6e8`, slot `+0x08` = `FUN_140f8bfd0`, `0x140f8bfd0`).
   - **Byte 0 (`u8` mode)**: `0x80` when uncompressed, or `0x81` when compressor `param_1[5] != 0`, `*(char *)((longlong)param_1 + 0x252) != 0`, and `*(int *)(conn + 0x68) != 0`. On receive, `FUN_140f898e0` checks `(char)byte0 < 0` and `(byte0 & 0x7e) == 0` (accepting only `0x80` or `0x81`).
   - **Byte 1 (`u8` format)**: `0x01`. On receive, `FUN_140f898e0` rejects the datagram if byte 1 is not `0x01`.
   - **Bytes 2 and 3 (`u16` datagram sequence)**: `*(uint16 *)(conn_state + 0x12f0) + 1`, byte-swapped to big-endian when `*(int *)(param_1 + 0x5a4) == 0` (carrier endian setting).
2. **Per-message frame header inside the datagram (`FUN_140f65b20` at `0x140f65b20` on send / `FUN_140f77eb0` at `0x140f77eb0` on receive)**:
   - `u8` flags byte:
     - bit `0x01`: reliable message (`1` = reliable, `0` = unreliable).
     - bit `0x04`: multi-chunk message (`1` = big-endian `u16` total chunk count follows the channel field).
     - bit `0x08`: implicit channel sequence (`1` = `prev_seq + 1`, `0` = explicit big-endian `u16` sequence follows).
     - bit `0x10`: implicit reliable sequence (`1` = `prev_rel_seq + 1`, `0` = explicit big-endian `u16` reliable sequence follows when bit `0x01` is set).
     - bit `0x20`: explicit channel ID (`1` = `u8` channel `0..3` follows the length field, `0` = same channel as the previous frame in the datagram, starting at channel `0`).
     - bit `0x80`: marker flag from `msg + 0x2a`.
   - Big-endian `u16` payload byte length (`msg + 0x28`).
   - Optional `u8` channel (`0..3`), optional big-endian `u16` chunk count, optional big-endian `u16` channel sequence, optional big-endian `u16` reliable sequence, and `length` payload bytes.
   - Channel `3` is the carrier system channel: the last byte of a channel-`3` frame payload (`payload[length - 1]`) is the system opcode (`6` = ACK bitmap written by `FUN_140f8ba70` at `0x140f8ba70` and parsed by `FUN_140f7bdd0` at `0x140f7bdd0`; `7` = 4-byte keepalive timestamp).
3. **LZ4 compression and decompression call sites**:
   - In `FUN_140f8ab70` (`0x140f8ab70`), when mode is `0x81`, the carrier copies the 4-byte datagram header into `*(uint32 *)param_1[0x25a5]` and invokes `(**(code **)(*(longlong *)param_1[5] + 0x28))(param_1[5], param_1[0x5a1] + 4, uVar19, param_1[0x25a5] + 4, &local_c8)`, which is virtual slot `+0x28` (`FUN_146ab3bd0`, `0x146ab3bd0`, calling `LZ4_compress_default` at `0x147970930`) on compressor `param_1[5]` (vtable `0x148588db8`). If compressed length `local_c8 < uVar19`, it sends `param_1[0x25a5]` (`local_c8 + 4` bytes); otherwise it clears bit 0 of byte 0 (`&= 0xfe` -> `0x80`) and sends the uncompressed buffer.
   - In `FUN_140f898e0` (`0x140f898e0`), when `(byte0 & 1) != 0` (`0x81`), the carrier invokes `(**(code **)(*plVar2 + 0x30))(plVar2, pbVar10 + 4, (int)uVar16 - 4, &local_c8, &local_a0)`, which is virtual slot `+0x30` (`FUN_146ab6540`, `0x146ab6540`, calling `LZ4_decompress_safe` at `0x1479714e0`) on `param_1[5]`.
4. **Handoff to the socket send queue**:
   - `FUN_140f8ab70` passes the finished datagram to virtual slot `+0x50` on `param_1[1]` (`FUN_140f7fd40` at `0x140f7fd40` on `0x147fbe628`, or the DTLS wrapper on `0x148480790`) and then calls virtual slot `+0x18` (`FUN_140f7fc60`, `0x140f7fc60`) at the end of `FUN_140f82340` to queue the `0x600`-byte slot and wake `GridMate-Carrier Packet Send Thread` (`FUN_140f8b810`, `0x140f8b810`).

Negative string searches, still empty: `SM_CONNECT`, `CONNECT_REQUEST`, `DATA_CHANNEL`, `kCarrierEndian`, `k_systemChannel`, `NewProxy`, `LZ4_decompress_safe`.

## 3. V3 registration

**Observed.** Three request types and one response type are registered by the same UUID parser (16 bytes, dashes and braces skipped). Each name string is followed by its UUID string:

| Name | UUID string | Registrar | File index (section 4) |
|---|---|---|---|
| `RegistrationRequestMsg` | `8673a3cc-2848-4c87-aa72-cc860589d1b5` | `0x1407f27f0` | 1 |
| `RegistrationRequestV2Msg` | `DA4E5889-A65C-4480-8642-0278160125A7` | `0x1407f2a20` | 2 |
| `RegistrationRequestV3Msg` | `0B826B33-89F5-49E0-B8CB-FE4433427778` | `0x1407f2c50` | 19 |
| `RegistrationResponseMsg` | `104145a7-ff95-44f1-9468-21fb41c8ac2b` | `0x1407f2e80` | 3 |

`FUN_1407f2c50` stores the parsed UUID and installs vtable `0x147f47278`. `FUN_1407f2e80` installs vtable `0x147f46910` for the response. Neither serializer references field-name strings. The order below is the order of writes and reads. Names are not in the binary.

**Observed.** Integer endian. `FUN_140876cd0` (`0x140876cd0`) loads a `uint32` and calls `0x1461656f0`, which jumps to the `WS2_32.dll` import at ordinal 8, then writes 4 bytes. `FUN_14087a220` (`0x14087a220`) reads 4 bytes and calls `0x146167950`, which jumps to `WS2_32.dll` ordinal 14. The same import table's other ordinals are 4, 9, 13, 15, 16, and 19. **Inferred:** ordinal 8 is `htonl` and ordinal 14 is `ntohl`, so these `uint32` fields are big-endian. The type-index prefix code in section 4 is a different encoding.

**Observed.** `RegistrationRequestV3Msg` write is vtable slot 4, `FUN_1407ce860` (`0x1407ce860`). The matching read is slot 5, `FUN_1407ce8e0` (`0x1407ce8e0`), which calls `FUN_1407d9620` (`0x1407d9620`). Write order:

1. Big-endian `uint32` from object `+0x8`.
2. `FUN_1407d62b0` (`0x1407d62b0`) on object `+0x10`. It writes a prefix-coded count (section 4). If the container length is above `0x2000000` it writes 0. Otherwise, for each list node, a big-endian `uint32` and then a length-prefixed byte string (`FUN_140876d80`, `0x140876d80`). A string longer than `0x2ffff` is replaced by a single 0 byte.
3. Length-prefixed string at object `+0xa0`. The length uses the same prefix code. A length of `0x30000` or more is written as 0 and the bytes are omitted (`FUN_1407d54a0`, `0x1407d54a0`).
4. `FUN_1407d5f30` (`0x1407d5f30`) on object `+0xc0`: one length-prefixed string, then `FUN_1407d4920` (`0x1407d4920`). That helper writes four length-prefixed strings and one big-endian `uint32`, then `FUN_1407d4b50` (`0x1407d4b50`). `FUN_1407d4b50` writes four more length-prefixed strings, then calls `FUN_1407d42c0` (`0x1407d42c0`). `FUN_1407d42c0` writes, in order: one length-prefixed string (length 0 when the string length is `0x30000` or more), three big-endian `uint32` values (`FUN_140876cd0`), one length-prefixed string, one big-endian `uint32`, one raw byte, one length-prefixed string, one byte that is 0 or 1, and, when that byte is 1, one more byte that is 0 or 1. It then calls `FUN_1407d46a0`, which is already recorded above.
5. On object `+0x2c8`: a big-endian `uint32`, one byte that is 0 or 1, another big-endian `uint32`, a length-prefixed string, then `FUN_1407d4540` (`0x1407d4540`): two length-prefixed strings, one big-endian `uint32`, two length-prefixed strings, then `FUN_1407d46a0` (`0x1407d46a0`). That helper writes one length-prefixed string, then `FUN_1407d4490` (`0x1407d4490`), which writes two length-prefixed strings and returns. That chain ends there.
6. `FUN_1407d3f70` (`0x1407d3f70`) on object `+0x3e0` and the byte at `+0x460`. Each of the two `FUN_140876fa0` (`0x140876fa0`) calls writes a flag byte. If bit 0 of that flag is set, the next field is 16 raw bytes. Otherwise it calls `FUN_1407d60d0` (`0x1407d60d0`), which writes one length-prefixed string (length 0 when the string length is `0x30000` or more). Then one byte, 0 or 1.

The read path checks the 0/1 byte and rejects a value of 2 or more. Field names are absent.

**Observed.** `RegistrationResponseMsg` write is `FUN_1407cceb0` (`0x1407cceb0`). The read is `FUN_1407cd040` (`0x1407cd040`). Order:

1. Big-endian `uint32` at object `+0x8`.
2. An 8-byte integer. `FUN_1408770e0` (`0x1408770e0`) writes it after `FUN_146165760` (`0x146165760`), which calls `WS2_32` ordinal 8 (`htonl`) on each 32-bit half. `FUN_14087ad90` (`0x14087ad90`) reads 8 bytes and calls `FUN_1461679c0` (`0x1461679c0`), which calls ordinal 14 (`ntohl`) on each half. **Inferred:** the field is a big-endian `uint64`. The name is absent.
3. Length-prefixed string at `+0x18` (`FUN_1407fc670`, `0x1407fc670`).
4. Length-prefixed string at `+0x38`.
5. Four bytes at `+0x58` through `+0x5b`. Each is written as 0 or 1. The reader (`FUN_1407da5e0`, `0x1407da5e0`) rejects a byte of 2 or more.

**Observed.** After the socket is up, the state names and logs are:

1. `StartREPConnection` logs `RepAddress` and `worldId` (`0x14644a070`).
2. String `GameConnectionWrapper: REP socket connection established, now waiting to register` (file `0x84fd140`).
3. `FUN_1464755e0` (`0x1464755e0`) logs `received registration response from REP`, then sets a value named `Server version` from the response argument.
4. `WaitingForActorGameConnection`, then `WaitingForSpawnPoint` (`spawn point found`), then `WaitingForPlayerSpawn` (`player spawn succeeds, in game`), then `InGame`.

**Observed.** The direct callers of the three registrars do not send a request. `FUN_1407c3120` (`0x1407c3120`) calls the V1 registrar and then `FUN_1461a9740` (the `Registered type` log). `FUN_1407c39e0` (`0x1407c39e0`) does the same for V2. `FUN_1407c3e20` (`0x1407c3e20`) does the same for V3. `FUN_14007a280` (`0x14007a280`) calls the V1 registrar (`FUN_1407f27f0`), `FUN_14007a450` (`0x14007a450`) calls the V2 registrar (`FUN_1407f2a20`), and `FUN_14007a620` (`0x14007a620`) calls the V3 registrar (`FUN_1407f2c50`), each passing its type name and UUID to `FUN_1461a9640` (`0x1461a9640`).

**Observed.** Three jump thunks forward to the registrars: `0x1407ce3a0` jumps to V1, `0x1407cf9b0` jumps to V2, and `0x1407cfca0` jumps to V3. Each jump address is stored once, at `0x147f47860`, `0x147f47920`, and `0x147f479d8`. Those three tables are embedded inside the message object vtables `0x147f47830` (V1), `0x147f478f0` (V2), and `0x147f479a8` (V3).

**Observed.** Which registration version the client sends (`FUN_146b6e190`, `0x146b6e190`):

1. When the REP socket connection completes, `FUN_146b6f190` (`0x146b6f190`) calls `FUN_146b6e190` (`0x146b6e190`).
2. `FUN_146b6e190` evaluates `if (DAT_149f80d34 == '\0' || FUN_146b6df50() != '\0')`:
   - `DAT_149f80d34` (`0x149f80d34`) is the boolean config flag for `use-auth-token-msg` (default `1` in `FUN_146168bc0`, `0x146168bc0`).
   - `FUN_146b6df50` (`0x146b6df50`) reads `client-connection.client-gateway.mode` and returns `1` when the mode string is `stubbed` or `dummy`, and `0` otherwise (including default `gateway` mode).
3. When `DAT_149f80d34 == 0` or gateway mode is `stubbed` / `dummy`, `FUN_146b6e190` logs `Client connection using V2 registration message type`, allocates `0x360` bytes, constructs `RegistrationRequestV2Msg` via `FUN_146b66a60` (`0x146b66a60`, vtable `0x147f478f0`), and sends it (`FUN_146b6e780`, `0x146b6e780`).
4. Otherwise (default `gateway` mode with `use-auth-token-msg` enabled), `FUN_146b6e190` logs `Client connection using authtoken V3 registration message type`, allocates `0x470` bytes, constructs `RegistrationRequestV3Msg` via `FUN_146b66820` (`0x146b66820`, vtable `0x147f479a8`), and sends it (`FUN_146b6e780`).
5. `RegistrationRequestMsg` (V1) is never sent by `FUN_146b6e190`.
6. **Observed sources of the six `RegistrationRequestV3Msg` field groups (`FUN_146b66820` at `0x146b66820` and `FUN_146425f20` at `0x146425f20`)**:
   - Group 1 (`+0x08`, `uint32`): type registry checksum from `FUN_146165190(TypeRegistryInstance)` (`0x146165190`).
   - Group 2 (`+0x10`, map of `(uint32, string)`): 6 protocol/layer hashes populated in `FUN_146425f20` by looping `i = 0..5` and calling `FUN_146161a20(local_68, i)` (`0x146161a20`).
   - Group 3 (`+0xa0`, string): string copied from `GameConnection + 0x10c0` (`REPConnection + 0x1c8`).
   - Group 4 (`+0xc0`, `0x208`-byte struct): the login-queue `Token` struct copied by `FUN_146403f10` (`0x146403f10`) from `GameConnection + 0x10e0` (`REPConnection + 0x1e8`), containing `Signature` (`+0x00`), `RepAddress` (`+0x20`), `WorldId` (`+0x40`), `CharacterId` (`+0x60`), `PersonaId` (`+0x80`), `SteamAppId` (`+0xa0`), `SteamUserId` (`+0xa8`), `ChannelId` (`+0xc8`), `TicketId` (`+0xe8`), `LocationGroupId` (`+0x108`), `LocationId` (`+0x128`), `GenerateTime` (`+0x148`), `IssueTime` (`+0x14c`), `AccountAge` (`+0x150`), `ClientCapabilities` (`+0x158`), `TokenVersion` (`+0x178`), `IsPermanentAppOwner` / `IsTrialOwner` bitfield (`+0x17c`), `HostHash` (`+0x180`), `AccountIsLocked` (`+0x1a0`, presence at `+0x1a1`), and `JwtClaims` (`+0x1a8`, plus two strings at `+0x1c8` and `+0x1e8`).
   - Group 5 (`+0x2c8`, `0x118`-byte struct): platform/auth-token requirements struct populated by `AuthManager` virtual slot `+0x90` in state `7` (`ObtainREPRequirements`) and passed at `REPConnection + 0x3f0`.
   - Group 6 (`+0x3e0`, `0x80`-byte struct, and `+0x460`, `u8` bool): impersonate/session struct (`REPConnection + 0x510`) and boolean flag (`REPConnection + 0x590`).
7. When `RegistrationResponseMsg` arrives in `FUN_146b6f190` (`0x146b6f190`), it checks `*(int *)(msg + 0x08) == 0`. On `0`, it logs `Client connection is authorized`, sets `*(char *)(rep_conn + 0x601) = 1`, and forwards the response to `GameConnection` (`FUN_1464755e0`, `0x1464755e0`).

## 4. Message type table

**Observed.** `typeindex.json` in the install has keys `generationTime` (`2025-10-22T03:13:16Z`) and `typeIndex` (7,052 strings of 32 hex digits, no dashes). Index 0 is 32 zero digits.

**Observed.** Loader `FUN_1461661c0` (`0x1461661c0`):

- Opens `typeindex.json`. On failure it logs `No type index file could be opened. Proceeding without index.` and returns 0.
- Requires a `generationTime` field and a non-empty `typeIndex` array (JSON type code 4). Otherwise it logs `typeindex failed validation`.
- Builds a vector of 16-byte values at object `+0x40`.
- The first slot is always the 16 zero bytes from `FUN_1407f7d80` (`0x1407f7d80`).
- For file index 0, the parsed UUID is appended only when it is not those 16 zero bytes. Later file entries are appended in order.
- So when file index 0 is the zero UUID, vector slot `i` is file entry `i`.

**Observed.** `FUN_14615f720` (`0x14615f720`) runs after a successful load. For each vector slot `i` with `i >= 1`, it looks up that UUID and writes `i` into the type object at `+0x54`. The registration log `Registered type: {uuid= … localIndex= …}` is `FUN_1461a9740` (`0x1461a9740`).

**Observed.** `GetTypeIdForIndex` is `FUN_1461acfe0` (`0x1461acfe0`). It reads the index with `FUN_14087b5c0` (`0x14087b5c0`):

- Index `0`: the next 16 bytes in the stream are the UUID (`FUN_140878610` with length `0x10`). The vector is not used.
- Index `> 0` and `<` vector length: the UUID is the 16 bytes at that slot.
- Otherwise it logs `Invalid index requested in GetTypeIdForIndex`.

The index integer is a prefix code, not standard LEB128 (research file 02 says LEB128; this function does not):

| First byte | Extra bytes | Value |
|---|---|---|
| `0x00` to `0x7F` | 0 | the byte |
| `0x80` to `0xBF` | 1 | low 6 bits of the first byte, extra byte shifted left 6 |
| `0xC0` to `0xDF` | 2 | low 5 bits, extra bytes shifted left 5 |
| `0xE0` to `0xEF` | 3 | low 4 bits, extra bytes shifted left 4 |
| `0xF0` to `0xFF` | 4 | low 3 bits, extra bytes shifted left 3 |

**Observed.** These file indexes match the UUID strings in the binary (hex digits only, case ignored):

| Index | Name |
|---|---|
| 0 | zero UUID (inline-UUID slot on the wire) |
| 1 | `RegistrationRequestMsg` |
| 2 | `RegistrationRequestV2Msg` |
| 3 | `RegistrationResponseMsg` |
| 19 | `RegistrationRequestV3Msg` |
| 209 | `PingRequestMsg` |
| 335 | `TimeSynchMsg` |
| 349 | `PingMsg` (registrar `0x1407f0940`, UUID `6a379fb8-0bdd-43a1-ab3e-9843d7be8cd3`) |
| 522 | `ReceiveActorMsg` (`b232749e-1c9b-4b96-bd9a-8ad04a23b6b4`) |
| 718 | `MoveActorToHubMsg` |
| 723 | `ProcessDeferredMovementRequestsMsg` |
| 919 | `DeleteActorMessage` |
| 921 | `NotifyActorMigrationCompletedMessage` (`764599E8-3D44-4EB3-BF44-77097CB414E3`) |
| 1008 | `PingMsg` (registrar `0x146aceec0`, UUID `519a3d71-f901-46b8-8969-f47b6fd492bc`) |
| 5867 | `CrossWorldReconnectNotifierTrait` (`{85C7E01E-55DE-4A6E-AFC5-80FA9A8AD260}`, adjacent to `PingMsg` in `.rdata`) |

The full UUID list is not in this repository.

**Conclusion.** For this install, `typeindex.json` is the `type_idx` → type UUID table used by `GetTypeIdForIndex`. Index 0 on the wire means "inline 16-byte UUID", and it is also the zero UUID in the file. Indexes `1` and above are file slots. This disproves the idea that the table is only a content-pipeline artifact. It does not by itself prove every gameplay message's field layout.

**Observed.** The writer that emits `localIndex` on the wire is `FUN_1461673b0` (`0x1461673b0`), the exact counterpart to `GetTypeIdForIndex` (`FUN_1461acfe0`):

1. If `*param_1 == 0`, it writes prefix-coded `0` (`FUN_140877970`, `0x140877970`) and returns.
2. If `*(int *)(*param_1 + 0x54) == 0` (`localIndex` not yet cached on the type descriptor), it looks up `*param_1 + 0x18` (the 16-byte UUID) in `TypeRegistryInstance` (`FUN_146162150` -> `FUN_14614a520`, `0x14614a520`).
3. It writes `localIndex` (`*(int *)(*param_1 + 0x54)`) as a prefix-coded integer via `FUN_140877970` (`0x140877970`).
4. If `localIndex != 0`, it returns immediately. If `localIndex == 0`, it writes the 16 raw UUID bytes from `*param_1 + 0x18` via `(*param_2 + 0x40)(param_2, *param_1 + 0x18, 0x10)`.
5. Callers of `FUN_1461673b0` include `FUN_146167110` (`0x146167110`, `MessageEnvelope` writer, the exact counterpart to `FUN_1461ace70`), `FUN_146166820` (`0x146166820`), `FUN_146b0f140` (`0x146b0f140`), `FUN_146ae6860` (`0x146ae6860`), and `FUN_1415a21f0` (`0x1415a21f0`).
6. In `MessageEnvelope` (`FUN_146167110` write / `FUN_1461ace70` read, registered by `FUN_146b108c0` at `0x146b108c0` with UUID `f4ab3b23-bd28-42c2-96d2-b929d9d2f2b5`), the low 3 bits of the first byte are presence flags for optional header fields on the envelope struct (`param_2`): bit `0` (`0x01`) = 8 raw bytes at `+0x04` (sender/source ID, checked against `FUN_14105e730()` for network traffic telemetry), bit `1` (`0x02`) = 8 raw bytes at `+0x0c` (recipient/destination ID), and bit `2` (`0x04`) = routing/actor-delivery header at `+0x18..+0x48` (big-endian `uint32`, two 16-byte UUIDs, a prefix-coded `uint64`, and an optional second prefix-coded `uint64`). After those optional headers, it writes a `0/1` presence byte (`*(param_2 + 0x60) != 0`), calls `FUN_1461673b0` to write the payload message's `localIndex` (or `0` + 16-byte UUID), and invokes the payload message's serializer (`vtable + 0x20`).

## 5. First world messages

**Observed.** These names sit in the message catalog with a UUID string in `.rdata`. The file index is the `typeindex.json` slot from section 4.

| Name | UUID | File index | Registrar | Writer / Reader |
|---|---|---|---|---|
| `MoveActorMsg` | `5570cfb7-0c5c-475e-bb19-d8fbc3fd8d32` | 710 | `FUN_146ac9d70` (`0x146ac9d70`) | `FUN_146a53fe0` / `FUN_146a54050` (`0x146a54050`) |
| `MoveActorToHubMsg` | `0a8f6818-915c-4700-9c33-463862bbe5f0` | 718 | `FUN_146ac9f90` (`0x146ac9f90`) | `FUN_146a546d0` (`0x146a546d0`) / `FUN_146a54740` (`0x146a54740`) |
| `StartMovementCommunicationMsg` | `767fc9bf-1c8d-417f-ad19-c51e363d4df0` | 726 | `FUN_146acfd90` (`0x146acfd90`) | `FUN_146a546d0` (`0x146a546d0`) / `FUN_146a55240` (`0x146a55240`) |
| `AckMovementMsg` | `5ad005ae-375c-4f2d-a00e-3ff9f6f01069` | 739 | `FUN_146ac15a0` (`0x146ac15a0`) | `FUN_146a55c70` / `FUN_146a55ce0` (`0x146a55ce0`) |
| `AddActorMessage` | `96C829CE-DFBC-4763-959C-32A6B22D81EF` | 121 | `FUN_146ac17c0` (`0x146ac17c0`) | `FUN_146a58ca0` / `FUN_146a58d20` (`0x146a58d20`, also `FUN_146a31f80`) |
| `ReceiveActorMsg` | `b232749e-1c9b-4b96-bd9a-8ad04a23b6b4` | 522 | `FUN_146acf9b0` (`0x146acf9b0`) | `FUN_146a568e0` (`0x146a568e0`) / `FUN_146a56950` (`0x146a56950`) |
| `NotifyActorMigrationCompletedMessage` | `764599E8-3D44-4EB3-BF44-77097CB414E3` | 921 | `FUN_146acaa70` (`0x146acaa70`) | `FUN_146a32620` (`0x146a32620`) / `FUN_146a32670` (`0x146a32670`) |
| `DeleteActorMessage` | `77CF6891-CE44-4F37-9E97-B8254E23D547` | 919 | `FUN_146ac4680` (`0x146ac4680`) | `FUN_146a5fe20` (`0x146a5fe20`) / `FUN_146a5ffd0` (`0x146a5ffd0`) |
| `TimeSynchMsg` | `038cd847-0653-4243-9a26-936e3bd7f312` | 335 | `FUN_1407f6a30` (`0x1407f6a30`) | `FUN_1407cc8c0` (`0x1407cc8c0`) / `FUN_1407cc8f0` (`0x1407cc8f0`) |
| `PingRequestMsg` | `ad4c28bd-4208-4b47-9679-9a46dd9e5287` | 209 | `FUN_1407f0b60` (`0x1407f0b60`) | `FUN_1407cda40` (`0x1407cda40`) / `FUN_1407cf9c0` (`0x1407cf9c0`) |
| `PingMsg` (#1) | `6a379fb8-0bdd-43a1-ab3e-9843d7be8cd3` | 349 | `FUN_1407f0940` (`0x1407f0940`) | `FUN_1407cda40` (`0x1407cda40`) / `FUN_1407cda70` (`0x1407cda70`) |
| `PingMsg` (#2) | `519a3d71-f901-46b8-8969-f47b6fd492bc` | 1008 | `FUN_146aceec0` (`0x146aceec0`) | `FUN_1407c0770` (`0x1407c0770`) / `FUN_146a3bcd0` (`0x146a3bcd0`) |
| `ProcessDeferredMovementRequestsMsg` | `4c220b64-c728-4d31-9c1e-6244e8089215` | 723 | `FUN_146acf530` (`0x146acf530`) | `FUN_140294860` (`0x140294860`) / `FUN_146a54fc0` (`0x146a54fc0`) |

**Observed.** Bodies that the decompiler shows:

- `MoveActorMsg` read (`FUN_146a54050`, `0x146a54050`) copies 16 raw bytes and then stops (`FUN_140878610` with length `0x10`).
- `MoveActorToHubMsg` (`FUN_146a546d0` write / `FUN_146a54740` -> `FUN_146a6b1b0` read) and `StartMovementCommunicationMsg` (`FUN_146a546d0` write / `FUN_146a55240` -> `FUN_146a6b1b0` read) share the same layout: 16 raw bytes at `+0x08`, then one big-endian `uint32` at `+0x18`.
- `DeleteActorMessage` (`FUN_146a5fe20` at `0x146a5fe20` write / `FUN_146a5ffd0` at `0x146a5ffd0` -> `FUN_146a6e170` at `0x146a6e170` read): 16 raw bytes at `+0x08`, followed by one `FUN_1415acc30` triple at `+0x18` (one big-endian `uint32`, 16 raw bytes, 16 raw bytes).
- `NotifyActorMigrationCompletedMessage` (`FUN_146a32620` at `0x146a32620` write / `FUN_146a32670` at `0x146a32670` read): 16 raw bytes at `+0x08`, followed by one big-endian `uint64` at `+0x18` (`FUN_14087ad90`).
- `TimeSynchMsg` (`FUN_1407cc8c0` at `0x1407cc8c0` write / `FUN_1407cc8f0` at `0x1407cc8f0` read): one big-endian `uint64` at `+0x08` (`FUN_146167320` / `FUN_1461acd90`).
- `PingRequestMsg` (`FUN_1407cda40` at `0x1407cda40` write / `FUN_1407cf9c0` at `0x1407cf9c0` read) and `PingMsg` #1 (`FUN_1407cda40` write / `FUN_1407cda70` at `0x1407cda70` read): one big-endian `uint64` at `+0x08` (`FUN_1408770e0` / `FUN_14087ad90`).
- `PingMsg` #2 (`FUN_1407c0770` at `0x1407c0770` write / `FUN_146a3bcd0` at `0x146a3bcd0` read): one `FUN_1415acc30` triple at `+0x08` (one big-endian `uint32`, 16 raw bytes, 16 raw bytes).
- `ProcessDeferredMovementRequestsMsg` (`FUN_140294860` write / `FUN_146a54fc0` at `0x146a54fc0` read): empty payload (0 wire bytes).
- `ReceiveActorMsg` read `FUN_146a56950` (`0x146a56950`) -> `FUN_146a6b3f0` (`0x146a6b3f0`) starts with one byte. A value of 2 or more fails. A value of 0 or 1 continues into `FUN_146a6a8a0` (`0x146a6a8a0`). A value of 1 first calls `FUN_146a86df0` (`0x146a86df0`), which allocates a `0xac0`-byte actor object and calls in-memory constructor `FUN_146a923f0` (`0x146a923f0`, which initializes fields and subobjects and reads no wire bytes). `FUN_146b074a0` (`0x146b074a0`) then reads:
  1. `FUN_146a6c940` (`0x146a6c940`) reads one byte that must be 0 or 1. A value of 2 or more fails. It then calls `FUN_146a6d390` (`0x146a6d390`), which reads:
     - Two `0/1` bytes (`+0x28` and `+0x29`).
     - `FUN_1415bc630` (`0x1415bc630`) -> `FUN_146b07360` (`0x146b07360`): one raw `u8` (`FUN_14087a190`), one length-prefixed string (`FUN_14087a3d0`), one big-endian `uint32` (`FUN_14087a220`), one `0/1` byte, and (when `*(char *)(param_3 + 0x3e) != 0`) one more `0/1` byte.
     - One `0/1` byte (`+0x60`), and when `1`, 16 raw bytes (`+0x64`).
     - Two length-prefixed strings (`FUN_1407fc670`, at `+0x78` and `+0x98`).
     - `FUN_146a6b260` (`0x146a6b260`): one length-prefixed string (`FUN_1407fc670`), one `0/1` byte (when `1`, reads two pairs of big-endian `uint32` values via `FUN_14087acf0` twice), three `FUN_1415acc30` triples (each: big-endian `uint32` + 16 raw bytes + 16 raw bytes), and one big-endian `uint32` (`FUN_14087a220`).
     Then `FUN_146a6c940` continues with:
     - `FUN_1461ac530` (`0x1461ac530`): calls `FUN_14158a220` (`0x14158a220`) -> `FUN_1461acca0` (`0x1461acca0`, which reads one `0/1` byte and, when `1`, reads a prefix-coded `uint64` via `FUN_14087b770` at `0x14087b770`), followed by one big-endian `uint64` (`FUN_14087ad90`).
     - `FUN_14087a820` (`0x14087a820`): reads a prefix-coded byte length (`FUN_14087b770`), resizes the byte vector at `+0x198`, and `memcpy`s that many raw bytes from the stream.
     - One `0/1` byte (`+0x1b0`).
     - `FUN_146a6b5b0` (`0x146a6b5b0`): reads a prefix-coded count (`FUN_14087b5c0`, max `0x2000000`) of entries (each: big-endian `uint32`, `0/1` byte, and `FUN_146a6db80` at `0x146a6db80` which reads a prefix-coded count of `FUN_1415acc30` triples), then calls `FUN_1461ac510` (`0x1461ac510`, two `FUN_14158a220` optional prefix-coded `uint64` values), reads four big-endian `uint64` values (`FUN_14087ad90`), calls `FUN_146a6da40` (`0x146a6da40`, prefix-coded count of `(big-endian uint64, 0/1 byte)` pairs), and calls `FUN_146a6bba0` (`0x146a6bba0`) -> `FUN_146a6b8e0` (`0x146a6b8e0`, prefix-coded count of `16 raw bytes + FUN_1415acc30 triple` entries, then a `FUN_146a6db80` vector, a `0/1` byte, a big-endian `uint64`, and a `0/1` byte).
  2. If the byte already stored at object `+0x408` is 0: reads 16 raw bytes (type UUID), calls `FUN_146abe4b0` (`0x146abe4b0`, which returns the `ActorFactoryInstance` singleton at `DAT_14a56dfd0 + 6`, reading no wire bytes) and `FUN_146ae35e0` (`0x146ae35e0`, which looks up that 16-byte UUID in `ActorFactoryInstance` and invokes factory slot `+0x10` to instantiate the actor component, reading no wire bytes), and then calls virtual slot `+0x28` on the created object to deserialize its state from the stream.
  3. If that byte is not 0: one byte that must be 0 or 1. A value of 1 calls `FUN_1461ad130` (`0x1461ad130`), the type-id reader in section 4, then virtual slots `+0x10` (allocate) and `+0x28` (deserialize) on the resolved type's serializer (`*(type_desc + 0x48)`).
  4. If the byte at object `+0xa0` is not 0: four big-endian `uint64` values.
  The common tail, for flag 0 and flag 1, is:
  1. 16 raw bytes, a big-endian `uint64`, a big-endian `uint64`, 16 raw bytes, a big-endian `uint32`, and one raw byte (`FUN_1461ac260`, `0x1461ac260`).
  2. A big-endian `uint32`, then 16 raw bytes, then 16 raw bytes (`FUN_1415acc30`, `0x1415acc30`).
  3. A big-endian `uint32`, a big-endian `uint32`, a big-endian `uint64`, and the same 8-byte big-endian integer as response field 2 (`FUN_146a6a5f0`, `0x146a6a5f0`, through `FUN_1461acd90`).
- `AddActorMessage` read `FUN_146a6a130` (`0x146a6a130`) then `FUN_146a6cde0` (`0x146a6cde0`):
  1. 16 raw bytes.
  2. One raw byte (`FUN_14087a190`, `0x14087a190`).
  3. One byte that must be 0 or 1 (`FUN_146a6d930`, `0x146a6d930`). A value of 1 calls `FUN_142d67640` (`0x142d67640`). That function calls `FUN_1461ad130`, the type-id reader in section 4, then virtual calls at offsets `0x10` and `0x28` on the type object. It reads no further integer of its own. A value of 0 reads nothing more in `FUN_146a6d930`.
  4. Two big-endian `uint64` values (`FUN_14087ad90`).
  5. One big-endian `uint64`.
  6. Two 4-byte values (`FUN_14087acf0`, `0x14087acf0`). Each 4 bytes is passed through `FUN_146167960` (`0x146167960`), which calls `WS2_32` ordinal 14. **Inferred:** ordinal 14 is `ntohl`, so each value is a big-endian `uint32`.
  7. One raw byte.
  8. Three length-prefixed strings (`FUN_14087a3d0`, `0x14087a3d0`). The length is the prefix code from section 4. A length of `0x30000` or more fails. The bytes that follow are raw.
  9. One big-endian `uint32`.
  10. One byte that must be 0 or 1.
  11. `FUN_1415b5870` (`0x1415b5870`): the three fields of `FUN_1415acc30` (one big-endian `uint32`, 16 raw bytes, 16 raw bytes), then one byte that must be 0 or 1.

**Observed.** `FUN_1469ec8b0` (`0x1469ec8b0`) reads grammar tokens `SpawnPlayerSpawner`, `SpawnEntity`, `SpawnCluster`, `ForceSpawnEntity`, and `Honeycomb`. `FUN_1469fccf0` allocates an object named `SpawnPlayerSpawner`. These three functions are not network message serializers.

**Observed.** `WaitingForPlayerSpawnPointMsg`, `ReceivePlayerSpawnPointMsg`, `DebugRequestSpawnPlayerMsg`, and `AddPortrayalToClientsMsg` occur only inside `InstallRegistrationHook` RTTI names. They do not have the plain name-plus-UUID catalog entry used above, and no field list was recovered for them. `PositionInTheWorldReplicatedState` is the same kind of RTTI name (`0x14a22c52e`).

**Observed.** How `FUN_14644a070` (`0x14644a070`) transitions from `WaitingForREPConnection` (`0xa`) through `WaitingForActorGameConnection` (`0xb`), `WaitingForSpawnPoint` (`0xc`), and `WaitingForPlayerSpawn` (`0xd`) to `InGame` (`0xe`):

1. **State `0xa` (`WaitingForREPConnection`) -> `0xb` (`WaitingForActorGameConnection`)**:
   - Polls virtual slot `+0xa8` on `*(param_1 + 0x1000)` (`REPConnection`), which returns `*(uint8 *)(rep_conn + 0x601)` (set to `1` in `FUN_146b6f190` at `0x146b6f190` when `RegistrationResponseMsg` has status `*(int *)(msg + 0x08) == 0`).
   - When `1`, `FUN_14644a070` initializes the `CJavelinActorGame` object at `param_1 + 0x130` via `FUN_145a905d0` (`0x145a905d0`) and enters state `0xb` (`WaitingForActorGameConnection`).
2. **State `0xb` (`WaitingForActorGameConnection`) -> `0xc` (`WaitingForSpawnPoint`)**:
   - Polls `FUN_145a92370(param_1 + 0x130)` (`0x145a92370`), which returns `*(int *)(actor_game + 0xa0) == 2`.
   - That field is set to `2` by `FUN_145a87010` (`0x145a87010`, logging `CJavelinActorGame` / `ConnectionSuccess`), which is called at the end of `FUN_146454c00` (`0x146454c00`, `GameMessagePort` handler logging `PlayerManagerSelfIdentification`).
   - When `*(int *)(actor_game + 0xa0) == 2`, `FUN_14644a070` logs `GameConnectionWrapper: actor game connection succeeds` and enters state `0xc` (`WaitingForSpawnPoint`).
3. **State `0xc` (`WaitingForSpawnPoint`) -> `0xd` (`WaitingForPlayerSpawn`)**:
   - Polls `FUN_145a905c0(param_1 + 0x130)` (`0x145a905c0`), which returns `*(uint8 *)(actor_game + 0xbc8)`.
   - In `FUN_146454c00` (`0x146454c00`, `PlayerManagerSelfIdentification`), `FUN_145a9fa30` (`0x145a9fa30`) writes `*(uint8 *)(actor_game + 0xbc8) = *(uint8 *)(param_7 + 6)` (via `this = actor_game + 0x78`, offset `+0xb50`).
   - When `*(uint8 *)(actor_game + 0xbc8)` is non-zero, `FUN_14644a070` logs `GameConnectionWrapper: spawn point found` and enters state `0xd` (`WaitingForPlayerSpawn`).
4. **State `0xd` (`WaitingForPlayerSpawn`) -> `0xe` (`InGame`)**:
   - Polls three conditions:
     1. `FUN_145a923c0(param_1 + 0x130)` (`0x145a923c0`), which returns `*(uint8 *)(actor_game + 0x252)`. That byte is set to `1` by `FUN_145a95160` (`0x145a95160`), which is called by `FUN_146448cd0` (`0x146448cd0`, `LoadContextAndLevel`, after both `PlayerManagerSelfIdentification` at `+0x1a0` and `LevelInfoChanged` at `FUN_146446800` / `+0x190` have arrived and the `map %s` command has been issued) and by `FUN_14644b980` (`0x14644b980`). `FUN_145a95160` also calls `FUN_146455e90` (`0x146455e90`, `ProcessPendingReliableMsgQueue`), which flushes queued reliable messages matching `typeindex.json` slots `8` (`8a40aec2-ae07-4f92-9bf3-78fc0cc94fdf`, `FUN_146437070` at `0x146437070`) and `9` (`fe59b513-ceb7-4bc2-80e6-545a8c492591`, `FUN_146437270` at `0x146437270`).
     2. `FUN_141026080()` (`0x141026080`) returns a non-null pointer (`*DAT_14a2fb9f0`).
     3. Virtual slot `+0x30` on that pointer returns non-zero (confirming the local player entity is spawned in the client context).
   - When all three conditions hold, `FUN_14644a070` logs `GameConnectionWrapper: player spawn succeeds, in game` and stores state `0xe` (`InGame`).

**Observed.** One level out from those strings: `FUN_14646d460` (`0x14646d460`) calls `FUN_14644a070` and names no message. `FUN_146446800` (`0x146446800`, `LevelInfoChanged`) logs `Update state %s to new state %s` with the `InGame` string and `WaitingForPlayerSpawn` when the connection state at `+0x1530` is `0xe`, then calls `FUN_14645fd70` (`0x14645fd70`) with `0xd` and stores `0xd`.

**Observed.** These names occur only inside mangled RTTI text. None of them has a code reference, and the bytes after the name are more mangled text, not a UUID catalog entry: `PlayerSpawnPointSuccessMsg`, `PlayerSpawnPointFailureMsg`, `PlayerSpawnPointMsg`, `SpawnPlayerMsg`, `WaitingForPlayerSpawnPointMsg`, `ReceivePlayerSpawnPointMsg`, `DebugRequestSpawnPlayerMsg`, `AddPortrayalToClientsMsg`. `NewReplica` and `CreateReplica` are absent. `FUN_1407f3500` (`0x1407f3500`) reads the string `Replicate`. `FUN_1407f3720` (`0x1407f3720`) reads `ReplicateClient`. Callers of `FUN_1407f3500` are `FUN_146a68f00`, `FUN_146a690d0`, `FUN_146a68d30`, and `FUN_146a70040`.

## Negative searches

Looked up and not found as strings: `No login ticket received`, `credentials/omni`, `jwks.json`, `openid-configuration`, `CreateSteamSessionJob`, `Received login ticket` (the build says `Received a login ticket:`), `type_idx`, `SM_CONNECT`, `CONNECT_REQUEST`, `DATA_CHANNEL`, `kCarrierEndian`, `NewProxy`, `LZ4` (uppercase token), `LZ4_decompress_safe`, `SSL_set_verify`, `SSL_VERIFY_PEER`, `d2i_X509`. `ClientOverride.json` exists and has no code reference from this search; the endpoint note covers the override file. `X509_STORE_add_cert` and `PEM_read_bio_X509` exist as OpenSSL name strings and have no direct code references; the client calls the un-named functions in section 1.4.

## Unknowns

All eight open items from the initial static pass are resolved above:

1. **Login-queue `Token` requirements and `Signature` check (resolved in section 1.3)**: `FUN_14643d110` (`0x14643d110`) logs `Received a login ticket: %s, location: %s, %s` on every non-null queue response using outer `TicketId` (`param_3 + 0x00`) and `QueueName` (`param_3 + 0x40`). To exit the queue and advance to REP connection setup, `Token` must be present (`*(char *)(param_3 + 0x278) != 0`) and `Token.Signature` (`param_3 + 0x70`) must be non-empty so `FUN_14114b200` (`0x14114b200`, `operator!=` against the initially empty `GameConnection + 0x10e0`) returns `true`. The client does not verify `Signature` cryptographically; it forwards the `Token` struct inside `RegistrationRequestV3Msg` (`+0xc0`).
2. **`RepAddress` host/port split (resolved in section 1.3)**: `FUN_140878e80` (`0x140878e80`, called via `FUN_140870880` from `FUN_146b6d600` at `0x146b6d600`) splits `RepAddress` on `':'` (`0x3a`) with `memchr`, classifies the host part as IPv4 or hostname with `WS2_32` ordinal 11 (`inet_addr`), and parses the port with `atoi`. If no port is present in `RepAddress`, `FUN_146b6d600` substitutes `client-connection.transport-options.rep-default-port` (default `33435`).
3. **Omni platform enum `1` (resolved in section 1.2)**: `FUN_14028fdb0` (`0x14028fdb0`) populates `DAT_14a829178` (read by `FUN_1479c23e0`, `0x1479c23e0`) with `1 = "steam"` (`0x147f48c88`), plus 13 other platform strings (`2` to `13` and `100`).
4. **Omni JWT verification (resolved in section 1.2)**: `FUN_1479c9f00` (`0x1479c9f00`) and `FUN_1479b6d00` (`0x1479b6d00`) treat `accessToken` as an opaque JSON string and do not decode or verify a JWT signature on the client.
5. **GridMate Carrier 4-byte datagram header and LZ4 call site (resolved in section 2)**: `FUN_140f8ab70` (`0x140f8ab70`, send) and `FUN_140f898e0` (`0x140f898e0`, receive) write and read byte 0 (`0x80` uncompressed / `0x81` LZ4-compressed), byte 1 (`0x01`), and bytes 2 and 3 (big-endian `uint16` datagram sequence), and invoke compressor vtable `0x148588db8` slots `+0x28` (`FUN_146ab3bd0`, `LZ4_compress_default`) and `+0x30` (`FUN_146ab6540`, `LZ4_decompress_safe`).
6. **Registration message version and V3 field sources (resolved in section 3)**: `FUN_146b6e190` (`0x146b6e190`) sends `RegistrationRequestV3Msg` (`FUN_146b66820`, `0x146b66820`) in default `gateway` mode when `use-auth-token-msg` (`DAT_149f80d34`) is non-zero, and falls back to `RegistrationRequestV2Msg` (`FUN_146b66a60`, `0x146b66a60`) only when `DAT_149f80d34 == 0` or gateway mode is `stubbed`/`dummy`. V1 is never sent.
7. **Transitions before `InGame` (resolved in section 5)**: `FUN_146454c00` (`0x146454c00`, `PlayerManagerSelfIdentification`) sets `*(int *)(actor_game + 0xa0) = 2` via `FUN_145a87010` (`0x145a87010`) and sets `*(uint8 *)(actor_game + 0xbc8)` via `FUN_145a9fa30` (`0x145a9fa30`). Once both `PlayerManagerSelfIdentification` and `LevelInfoChanged` (`FUN_146446800`, `0x146446800`) have been received, `FUN_146448cd0` (`0x146448cd0`, `LoadContextAndLevel`) and `FUN_14644b980` (`0x14644b980`) call `FUN_145a95160` (`0x145a95160`), which sets `*(uint8 *)(actor_game + 0x252) = 1` (`FUN_145a923c0`) and flushes pending reliable messages (`FUN_146455e90`, `0x146455e90`). State `0xd` (`WaitingForPlayerSpawn`) then advances to `0xe` (`InGame`) as soon as `FUN_141026080()` (`0x141026080`) virtual slot `+0x30` reports the local player entity is spawned.
8. **`localIndex` writer (resolved in section 4)**: `FUN_1461673b0` (`0x1461673b0`) writes `localIndex` (`*(int *)(*param_1 + 0x54)`) with `FUN_140877970` (`0x140877970`) and, when `localIndex == 0`, appends the 16 raw UUID bytes from `*param_1 + 0x18`.

**Remaining unknown**: C++ source field names for the unnamed members inside `RegistrationRequestV3Msg`, `RegistrationResponseMsg`, `ReceiveActorMsg`, and `AddActorMessage`, and the plain C++ class names for `typeindex.json` slots `8` (`8a40aec2-ae07-4f92-9bf3-78fc0cc94fdf`) and `9` (`fe59b513-ceb7-4bc2-80e6-545a8c492591`), which are stripped from this retail binary.

## Implications for the preservation login server

The stock client, after it has a channel config, gets a Steam auth-session ticket and sends that ticket on the auth-stack `/steam` call (or to Omni `GetOrCreateToken` with `platformIdentityType: "steam"` and `platformAuth.ticket`) and as `SteamAuthTicket` inside `LoginQueueRequest`. The Omni `accessToken` is stored as an opaque string without client-side JWT signature verification and is sent in the `x-nw-auth` header when the gateway host contains `.cloudfront.net` (`FUN_14646fca0`).

The queue reply the client parses is an object `LoginQueueResponse`. Queue position, `RefreshInterval`, `QueueName`, and outer `TicketId` are on that object. `RepAddress`, `Signature`, `TicketId`, `WorldId`, `CharacterId`, `PersonaId`, `HostHash`, and `JwtClaims` are strings (and `GenerateTime`, `IssueTime` are JSON numbers) on the nested `Token` object. To leave the login queue and start the REP connection, `Token` must be present with a non-empty `Signature` string and a non-empty `RepAddress` (`<host>:<port>` or `<host>`, defaulting to port `33435`). The client does not verify `Token.Signature` locally; it forwards the entire `Token` struct inside `RegistrationRequestV3Msg` (`+0xc0`) over the REP connection.

The game-server path for `gridmate-udp` and `gridmate-udp-bsd` is given the embedded PEM and a cipher list of `ECDHE-RSA-AES256-GCM-SHA384`. No launch argument or config key in the endpoint note replaces that certificate. Over that carrier, each datagram starts with a 4-byte header (`0x80` uncompressed or `0x81` LZ4-compressed, `0x01`, big-endian `uint16` sequence) written by `FUN_140f8ab70` and read by `FUN_140f898e0`. In default `gateway` mode, the client sends `RegistrationRequestV3Msg` (`file index 19`) and expects `RegistrationResponseMsg` (`file index 3`) with status `uint32 == 0` at `+0x08`.

`typeindex.json` from this install is the type-index table that both the message reader (`FUN_1461acfe0`) and the message writer (`FUN_1461673b0`) use. A preservation server that speaks this build's messages has to use that same index, with index 0 meaning a following 16-byte UUID.
