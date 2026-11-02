# Login unknowns

Date: 2026-09-02
Build: Steam build `22469132`, `Bin64/NewWorld.exe`, image base `0x140000000`, x86-64, Windows compiler spec. Ghidra project `<re-scratch>/ghidra` (program `NewWorld.exe`).
Method: static analysis only, on the already-analyzed project, opened read-only with analysis left off (`analyzeHeadless -process NewWorld.exe -noanalysis -readOnly`). One headless script (`<re-scratch>/scripts/Unknowns.java`) used a pool of 16 `DecompInterface` instances and a 24 GB heap (`_JAVA_OPTIONS=-Xmx24g`). It decompiled 280 functions in 62 seconds into `<re-scratch>/out-unknowns`. Earlier dumps under `<re-scratch>/out-20260902`, `<re-scratch>/out-followups`, and `/tmp/nw-shapes` were read first. A few call sites were disassembled from the executable with `objdump`. The game was not run. No game file and no Ghidra project file was changed. Easy Anti-Cheat was not analyzed. Static analysis did not observe execution.

REA `binary_session` with `{}` reported `open: false`. `open_binary` imports the executable into a new project and runs auto-analysis, so REA was not attached to this project. The rest of this note is from the existing Ghidra project.

Labels: **Observed** = an address, a string, or decompiler output in this build. **Inferred** = a conclusion from that evidence. **Unknown** = not established.

Related notes:

- [2026-09-02 login flow JSON shapes](2026-09-02-login-json-shapes.md)
- [2026-09-02 login flow follow-ups](2026-09-02-login-followups.md)
- [2026-08-29 client protocol: login, GridMate, and world entry](2026-08-29-client-protocol-login-gridmate.md)

## 1. `platformMetadata` key strings

**Observed.** `FUN_1408e8550` (`0x1408e8550`) concatenates two strings. The prologue saves `R8` in `R14`, `RDX` in `RSI`, and `RCX` in `RBP`. It copies the `RDX` string, then calls `FUN_1402c4260` (`0x1402c4260`) with that copy as the destination and `R14` (the original `R8`) as the text to append. The result is copied to the `RCX` destination.

**Observed.** The OMNI getter `FUN_146437fe0` (`0x146437fe0`) calls it at `0x14643806e` with `RDX` = `OMNI.` (`0x148501274`). The 14 instructions before that call do not write `R8`, and the getter prologue does not write `R8` either. The PERSONA getter `FUN_146439090` (`0x146439090`) does the same at `0x1464390c3` with `RDX` = `PERSONA.` (`0x148501220`). The suffix is the `R8` value the caller of the getter left in place.

**Observed.** `.BRANCH` retry, inside `FUN_146437fe0` only (the PERSONA getter has no second lookup). When the first exact lookup misses and the key length is greater than 6, the getter scans for `'.'` with `memchr`. At each hit it compares:

```
(*piVar5 == 0x4152422e) && ((short)piVar5[1] == 0x434e) && (*(char *)((longlong)piVar5 + 6) == 'H')
```

`0x4152422e` is `.BRA`, `0x434e` is `NC`, and `'H'` completes `.BRANCH` (string `0x1485012d0`). The match offset becomes the new length. The shorter key is hashed again (FNV-1a `0xcbf29ce484222325`) and looked up. Text from `.BRANCH` to the end is not part of the second key.

**Observed.** Both getters are slots on the channel-service vtable at `0x1484fab80`:

| Slot | Function | Role |
|---|---|---|
| `+0x68` | `FUN_146439090` | PERSONA lookup |
| `+0x70` | `FUN_146437fe0` | OMNI lookup |
| `+0x78` | `FUN_146430df0` | channel-key builder |

There is no direct `call` to either getter. `FUN_1408e8550` has many other direct callers. Their `RDX` immediates are other prefixes (`0x147fc9578`, `0x148042fd0`, `0x1481c0920`, and others). Those calls are not the metadata lookup.

**Unknown.** The runtime string passed in `R8` at the virtual call. No callsite in the disassembled windows loads a literal suffix such as a channel name. The full key is `OMNI.<that string>` or `PERSONA.<that string>`.

Partially resolved.

## 2. `omniTokenUrl` or `nwTokenUrl` into `FUN_1479c9160`

**Observed.** `FUN_1464211f0` (`0x1464211f0`) stores four strings on the OMNI object:

| Offset | Key | String address |
|---|---|---|
| `+0x00` | `omniStage` | `0x148501280` |
| `+0x28` | `omniGameAlias` | `0x148501290` |
| `+0x50` | `omniTokenUrl` | `0x1485012a0` |
| `+0x78` | `nwTokenUrl` | `0x1485012b0` |

The getter copies those four slots to the output at the same offsets (`puVar10 + 7`, `+0xc`, `+0x11`, `+0x16`).

**Observed.** The only caller of `FUN_1479c9160` (`0x1479c9160`) is `FUN_1479bc430` (`0x1479bc430`, `module_provider.cpp`). The call is `FUN_1479c9160(object, *param_2, *(int *)(param_2 + 4))`. The second argument is the game alias. The third is the stage integer. `FUN_1479c9160` then writes the host from that integer only:

| Stage | Host | Address |
|---|---|---|
| `1` | `https://gamma.tokenservice.amazongames.com` | `0x1495da170` |
| `2` | `https://beta.tokenservice.amazongames.com` | `0x1495da140` |
| `0` or any other | `https://tokenservice.amazongames.com` | `0x1495da1a0` |

The path is `/games/%s/tokens` (`0x1495da1f8`). The `%s` is the alias argument, not a URL from the channel document.

**Observed.** The string `omniTokenUrl` and the string `nwTokenUrl` have references only inside `FUN_1464211f0`. `FUN_1479bc430` does not write the host field again after `FUN_1479c9160` returns.

**Inferred.** Neither `omniTokenUrl` nor `nwTokenUrl` is copied into the host used by `FUN_1479c9160`. The host is the stage default. Both URL strings are stored and returned by the getter, and no bound function applies them to that host.

Resolved.

## 3. Order of `/steam`, persona `GET /user`, and the Omni token call

**Observed.** In `FUN_145a936f0` (`0x145a936f0`) the two paths are alternatives, selected by the int at object `+0x2ec`.

When that int is `0` (`0x145a93f00` region through `0x145a94185`):

1. The callback logs the Steam ticket and appends `/steam` (`0x14844bbb8`).
2. `FUN_145a88e40` (`0x145a88e40`) builds the body. `FUN_141722280` (`0x141722280`) sends it.
3. `FUN_145a8ef80` (`0x145a8ef80`) parses the reply (`0x145a940b5`).
4. `FUN_145a9f360` (`0x145a9f360`) is called at `0x145a940fe` with the tag string at `0x14844b540` (`auth stack`).
5. The function jumps to `0x145a9424c` and does not enter the persona block.

When that int is not `0` (`0x145a9418a`):

1. The persona client is the pointer at `+0x228` (`param_1[0x45]`).
2. If the pointer is null, or the virtual method at `+0x10` returns non-zero, the client logs `Persona failed to initialize!` (`0x14844bbd8`) and calls `FUN_145a9ae20` (`0x145a9ae20`) with `@mm_authresult_Error_Persona` (`0x14844bbf8`).
3. Otherwise it calls virtual `+0x58`, then `+0x20` or `+0x28` with the ticket string and the Steam id string. That path is the persona `GET /user` client (`FUN_1479c6720`, section 11). It does not send `/steam`.

**Observed.** The Omni session creator `FUN_1479b3cb0` (`0x1479b3cb0`, `CreateSession starts` at `0x1495d7230`) has one direct caller: `FUN_145a97310` (`0x145a97310`) at `0x145a9738d`. `FUN_145a97310` itself has no direct `call`. It passes a platform id, two strings, and an optional fallback token into `FUN_1479b3cb0`.

**Unknown.** Which branch of `FUN_145a936f0` runs on the normal login, because the writer of the int at `+0x2ec` was not bound. The call that reaches `FUN_145a97310` was not bound, so the Omni token call is not placed before or after the persona call on one straight-line path.

Partially resolved.

## 4. `GetLoginInfo` body `FUN_1474ea9b0` and `CharacterCreationParams`

**Observed.** `FUN_1474ea9b0` (`0x1474ea9b0`) writes two optional strings and nothing else:

- `channelId` (`0x148702f20`) when the flag at `+0x1a8` is set, from the string at `+0x188`.
- `tokenVersion` (`0x148703c18`) when the flag at `+0x2a0` is set, from the string at `+0x280`.

The only reference to the function is the pointer stored at `0x148703590`. There is no direct `call`.

**Observed.** `CharacterCreationParams` (`0x148703a28`) is written by `FUN_1474ec2c0` (`0x1474ec2c0`) as one string, when the flag at `+0x20` is set. The value is the string object at the start of the same struct (`FUN_147465da0`). The same function also writes `ClientCapabilities`, `Name` (`0x147f020a8`, the `DAT_147f020a8` argument), `SkipFtue`, `SteamAppId`, and `SteamAuthTicket`. `FUN_1474ecf20` (`0x1474ecf20`) wraps that object as `CreateCharacterRequest` (`0x148703a28` region, literal in the decompiler at the call `FUN_147465bf0`).

**Unknown.** The function that sets the `+0x1a8` / `+0x2a0` flags and then calls through `0x148703590`. The function that fills the `CharacterCreationParams` string before `FUN_1474ec2c0` reads it. That string is copied, not built, in `FUN_1474ec2c0`.

Partially resolved.

## 5. Optional `Character` object from `FUN_1474ecbc0`

**Observed.** `FUN_1474ecbc0` (`0x1474ecbc0`) builds one JSON object. If the flag at `+0x4f8` is set, it writes a `Character` member (`0x147f837c8`) by calling `FUN_1474ebec0` (`0x1474ebec0`) on the struct at `+0x188`. If the flag is clear, the object has no `Character` member. The only reference is the pointer at `0x148703560`. There is no direct `call`.

**Unknown.** The request that invokes that pointer. The neighboring pointers at `0x148703568` (`FUN_1474eb860`) and `0x148703590` (`FUN_1474ea9b0`) do not name the route.

Partially resolved.

## 6. `ValidateCharacter` reply body

**Observed.** `FUN_1463e68a0` (`0x1463e68a0`) has two direct callers: `FUN_146419ef0` (`0x146419ef0`, path `/game/worlds/%s/characters/validator`) and `FUN_1464148e0` (`0x1464148e0`, path `/game/characters/%s`). On the success flag (`local_4c0 != 0`) it constructs a `CommandData`, calls `FUN_1461b3240` (`0x1461b3240`), and sets byte `+0x68` to `1`. It reads no JSON key. On failure it records the HTTP status and does not parse a model either. The HTTP result buffer is released by `FUN_1461b4870` (`0x1461b4870`) before the function returns.

**Inferred.** HTTP success is the whole reply. There is no reply body for the client to read.

Resolved: no body.

## 7. Date parser format argument `2`

**Observed.** `FUN_147466d90` (`0x147466d90`) argument 3 is the format. Format `2` calls `FUN_147467960` (`0x147467960`, `RFC822DateParser`) first. If that parser's fail flag is clear, the RFC 822 result is kept. If the fail flag is set, it calls `FUN_147467550` (`0x147467550`, `ISO_8601DateParser`). A successful ISO parse leaves the UTC flag at its initial value `1`, and the wrapper uses `_mkgmtime64`.

**Observed.** `FUN_147467550` is a state machine on the character and on the count of characters since the last separator (`uVar6 - uVar7`). A string of length `0x65` (101) or more is rejected (`Incoming String to parse too long with len ` at `0x1486f89b8`). Success requires the state at `+0x48` to be `7` and the error flag at `+0x8` to be clear.

| State | Accept | Next state |
|---|---|---|
| `0` | digit, or `'-'` after 4 digits | `1` on `'-'` |
| `1` | digit, or `'-'` after 2 digits | `2` on `'-'` |
| `2` | digit, or `'T'` after 2 digits | `3` on `'T'` |
| `3` | digit, or `':'` after 2 digits | `4` on `':'` |
| `4` | digit, or `':'` after 2 digits | `5` on `':'` |
| `5` | `'Z'` after exactly 2 digits, or `'.'` after exactly 2 digits, or a digit | `7` on `'Z'`, `6` on `'.'` |
| `6` | `'Z'`, or a digit | `7` on `'Z'` |

State `5` with `'Z'` but not exactly 2 digits falls through to `isdigit`. `'Z'` fails that test and sets the error flag. State `6` digits are tested with `isdigit` and are not written into the `tm`. Any other character, including `'+'` and `'-'` after the time, sets the error flag.

**Inferred.** Format `2` accepts an ISO 8601 timestamp only when it ends in `Z` after `YYYY-MM-DDTHH:MM:SS` or after `YYYY-MM-DDTHH:MM:SS.` plus fractional digits. A numeric offset is rejected. Fractional seconds are accepted and discarded. A form with no `Z` never reaches state `7` and fails.

Resolved.

## 8. Character-select name join

**Observed.** `FUN_14641b020` (`0x14641b020`) walks character records of `0x420` bytes. It keeps a record when `memcmp` (`0x14641b020` at the compare near the `0x420` stride) says the world-id field equals the selected world id. It then `_stricmp`s one string on the record with the string at `GameConnection + 0x1050`. It does not read `DisplayName`, `OwningResourceId`, `IsNameLatent`, or `OwnerState`.

**Observed.** `OwningResourceId` (`0x148702f08`) is read only by `FUN_1474e7090` (`0x1474e7090`), which stores it at name-record `+0x168` and sets the flag at `+0x188`. `IsNameLatent` (`0x148702c68`) is read by `FUN_1474e1f60` (`0x1474e1f60`) and `FUN_1474ebec0` (`0x1474ebec0`). `displayName` (`0x148156be8`) is a script-bind name in `FUN_144f40780` (`0x144f40780`).

**Unknown.** The function that pairs a `NameReservations` record with a `Characters` record. No compare of `OwningResourceId` to `CharacterId` was found. **Inferred** from `FUN_14641b020` alone: the tile list is the `Characters` records whose world id matches. A non-default name field is not required by that filter.

Partially resolved.

## 9. Gateway non-200 UI strings

**Observed.** `FUN_141003620` (`0x141003620`) turns the reason int into the text logged by `FUN_146460240` (`0x146460240`) after `Report Error: Reason: ` (`0x1484feb08`). The jump table is at `0x141003b68`. Names used by the login routes:

| Reason | Text |
|---|---|
| `0x02` | `Errors::login_bad_request` |
| `0x03` | `Errors::login_world_maintenence` |
| `0x04` | `Errors::login_invalid_capability` |
| `0x05` | `Errors::login_forbidden_error` |
| `0x06` | `Errors::login_invalid_ticket` |
| `0x07` | `Errors::login_discarded_ticket` |
| `0x08` | `Errors::login_bad_ticket` |
| `0x09` | `Errors::login_unable_to_connect` |
| `0x0a` | `Errors::login_no_connection` |
| `0x0b` | `Errors::login_queue_join_failed` |
| `0x0d` | `Errors::login_failed_to_create_character` |
| `0x0f` | `Errors::login_character_list_failed` |
| `0x10` | `Errors::login_world_list_failed` |
| `0x11` | `Errors::login_no_worlds` |
| `0x12` | `Errors::login_no_worlds_unknown` |
| `0x14` | `Errors::login_steam_auth_required` |

**Observed.** Operation id written at request-context `+0x84`:

| Id | Builder | Route |
|---|---|---|
| `0` | `FUN_1464154e0` (`0x1464154e0`) | `/game/getlogininfo` |
| `1` | `FUN_146414350` (`0x146414350`) | `/game/worlds/%s/characters` |
| `3` | `FUN_146417490` (`0x146417490`) | `/game/login/queue/v2` |
| `4` | `FUN_146417a70` (`0x146417a70`) | refresh queue |
| `5` | `FUN_146419ef0` (`0x146419ef0`) | `/game/worlds/%s/characters/validator` |
| `0xd` | `FUN_146416ab0` (`0x146416ab0`) | `/game/worlds/list` |

The HTTP status is stored at `+0x80` by those builders. None of them switch on that status.

**Observed.** Queue handler `FUN_14643d110` (`0x14643d110`), called from `0x146474f49` and `0x146475129`, switches on the status int at `param_4[10]`. It does not read `+0x84`.

| Status | Reason | Extra test |
|---|---|---|
| `-1` | `0x09` `Errors::login_unable_to_connect` | none |
| `400` | `0x02` `Errors::login_bad_request` | none |
| `403` (`0x193`) | `0x03` `Errors::login_world_maintenence` | body contains `WorldMaintenance` |
| `403` | `0x04` `Errors::login_invalid_capability` | body contains `InvalidCapability` |
| `403` | `0x05` `Errors::login_forbidden_error` | body contains `AccessDenied` |
| `403` | `0x14` `Errors::login_steam_auth_required` | body contains `SteamAuthRequired` |
| `404` (`0x194`) | `0x06` `Errors::login_invalid_ticket` | none |
| `409` (`0x199`) | `0x07` `Errors::login_discarded_ticket` | none |
| `410` (`0x19a`) | `0x08` `Errors::login_bad_ticket` | none |
| `429` (`0x1ad`) | no `FUN_146460240` call | branch jumps past the reason |
| `502` (`0x1f6`) | `0x0a` `Errors::login_no_connection` | none |
| any other | `0x0b` `Errors::login_queue_join_failed` | none |

When the follow-up flag `param_5` is set, the same function calls the UI with `@mm_queuerefresherror`.

**Observed.** Worlds list, in `FUN_14641b020`, does not read the HTTP status. A null result uses reason `0x10`. An empty world range uses reason `0x11`. A world that fails `FUN_1464460d0` uses reason `0x12` and the text `(%s, %d)` (`0x1484fe` region, format in the decompiler). GetLoginInfo failure in `FUN_1464154e0`, when the status is `-1` and the message is empty, writes the literal `NoConnection` (`0x63656e6e6f436f4e`) into the error string. It does not call `FUN_146460240` itself.

**Unknown.** The status switch, if any, for getlogininfo (`+0x84 == 0`), worlds/list (`0xd`), characters (`1`), and the validator (`5`). The localization key that the UI shows for each `Errors::` name, other than `@mm_queuerefresherror` on the queue refresh path.

Partially resolved.

## 10. CMS fetch failure

**Observed.** `FUN_14644bc60` (`0x14644bc60`) logs `CGame::OnCampfireLoginComplete, login successful.` (`0x1484fd018`) and then writes three cvars through virtual `+0x48`:

| Cvar | Format | Address |
|---|---|---|
| `g_worldsCMSKey` | `worlds_%s.json` | `0x147fc9148`, `0x147fc9158` |
| `g_worldsCMSMarketingKey` | `marketingtiles/%s/metadata.json` | `0x147fc9168`, `0x147fc90e0` |
| `g_worldsCMSNewsStoriesKey` | `newsstories/%s/metadata.json` | `0x147fc9180`, `0x147fc9128` |

The function does not call `FUN_146460240` and does not test an HTTP status. `OnWorldCMSDataSet`, `OnMarketingTilesCMSDataSet`, and `OnNewsStoriesCMSDataSet` (`0x14831c1e0`, `0x14831c1f8`, `0x14831c218`) are registered as event names in `FUN_1441d5100` (`0x1441d5100`) and `FUN_1441ee020` (`0x1441ee020`). The host `https://d1hkbwzm1bktgo.cloudfront.net` (`0x1484cc6c8`) is referenced from `FUN_14616ae90` (`0x14616ae90`), the default-host table.

**Inferred.** This completion path does not wait for the CMS body and does not fail the login when the fetch fails. **Unknown.** A handler that would still block character select on that HTTP error was not found.

Partially resolved.

## 11. Persona `GET /user` keys and a missing `PERSONA.` entry

**Observed.** `FUN_1479c6720` (`0x1479c6720`) sends `GET` with path `/user` (`0x1495d9bd0`) and header `Authorization` (`0x1482e7f18`). On a JSON body it reads:

| Key | Address | Required |
|---|---|---|
| `errorCode` | `0x1486f4700` | if present, the success keys are not read; an unknown value logs `Unrecognized response error code: %s` (`0x1495d8020`) |
| `id` | `0x147f0614c` (pointer slot `0x14a12a1b8`) | yes, when `errorCode` is absent; missing calls `FUN_1479c4700` |
| `displayName` | `0x148156be8` | yes, same rule |
| `base` | `0x147f4da44` (pointer slot `0x14a12a1c8`) | no; looked up on the `displayName` value |
| `discriminator` | `0x1495d9ae8` | no; same object as `base` |
| `displayNameLastUpdateReason` | `0x1495d9af8` | no; looked up on the top object and mapped through a hash table |

**Observed.** `FUN_1479c4560` (`0x1479c4560`), called from `FUN_1479bc430`, sets the identity host from the stage integer: `https://prod.identity-service.amazongames.com` (`0x1495d9a28`), `https://gamma.identity-service.amazongames.com` (`0x1495d99f8`), or `https://beta.identity-service.amazongames.com` (`0x1495d99c8`). It also copies a per-stage hex key (`0x1495d9a89`, `0x1495d9ab9`, `0x1495d9a59`). It does not read `personaHostV2`. That string (`0x148501240`) is referenced only from `FUN_146421a60` (`0x146421a60`).

**Observed.** The stop in `FUN_145a936f0` (section 3) is a null persona client or a not-ready result from virtual `+0x10`. That block does not test the channel document.

**Inferred.** A missing `PERSONA.` entry does not select the fail string by itself. The identity gateway, once constructed, uses `identity-service.amazongames.com` for the stage. **Unknown.** The function that would copy `personaHostV2` over that host, and whether a missing `PERSONA.` entry leaves the client pointer null.

Partially resolved.

## 12. Javelin.RPC protobuf wire encoding

**Observed.** The binary links protobuf 3.14.0. File-path strings include `google\protobuf\wire_format_lite.cc` (`0x1495ac060`) and `J:\Javelin\Mainline\3rdParty\protobuf\protobuf-3.14.0\...` (`0x1486f61c0`). RTTI `.?AVMessageLite@protobuf@google@@` is at `0x14a281270`. `Javelin.RPC.*` descriptor names sit at `0x1484c18a0` and the following strings. `FUN_147852e40` (`0x147852e40`, 618 bytes) is the function at the `wire_format_lite.cc` path. It formats `String field '%s' contains invalid UTF-8 data`. It does not write a tag or a varint.

**Unknown.** The function that writes a `Javelin.RPC` message to the wire. No `SerializeToArray` or `ByteSizeLong` string exists. The generated writers were not separated from the descriptor data, and this pass did not decompile a large generated serializer to find them.

Still unknown. The encoder was not isolated. The nearest named function is `FUN_147852e40`, and it is the UTF-8 check, not the encoder.

## Negative searches

Looked up and not found in this pass:

- A direct `call` to `FUN_146437fe0` or `FUN_146439090`. Both are vtable slots only.
- A literal suffix concatenated with `OMNI.` or `PERSONA.` at a callsite of `FUN_1408e8550`.
- A reference to the strings `omniTokenUrl` or `nwTokenUrl` outside `FUN_1464211f0`.
- A direct `call` to `FUN_1474ea9b0` or `FUN_1474ecbc0`. Both are data pointers only.
- A JSON field read on the success path of `FUN_1463e68a0`.
- A compare of `OwningResourceId` with `CharacterId` in `FUN_14641b020`.
- A `FUN_146460240` call, or an HTTP-status test, in `FUN_14644bc60`.
- `SerializeToArray`, `SerializeToString`, `ByteSizeLong`, or `_InternalSerialize` as strings.

## Unknowns

1. The runtime suffix appended to `OMNI.` and `PERSONA.` (section 1).
2. Where `omniTokenUrl` and `nwTokenUrl` are read after `FUN_1464211f0` stores them (section 2). They are not the host of `FUN_1479c9160`.
3. Which value of the int at `FUN_145a936f0 + 0x2ec` the normal login uses, and the caller of `FUN_145a97310` (section 3).
4. The caller of the `channelId` + `tokenVersion` writer, and the bytes placed in `CharacterCreationParams` (section 4).
5. The request that calls `FUN_1474ecbc0` (section 5).
6. The join from `NameReservations.OwningResourceId` to a `Characters` record (section 8).
7. The HTTP-status switch for getlogininfo, worlds/list, characters, and the validator, and the UI text for each `Errors::` name other than `@mm_queuerefresherror` (section 9).
8. A CMS HTTP-failure handler, if one exists outside `FUN_14644bc60` (section 10).
9. The function that copies `personaHostV2` onto the identity gateway (section 11).
10. The `Javelin.RPC` protobuf writer (section 12).

## Second pass

Method: one more read-only headless run (`<re-scratch>/scripts/Pass2.java`), 28 `DecompInterface` instances, 24 GB heap, analysis left off. It decompiled 92 functions into `<re-scratch>/out-pass2`. Call sites that the decompiler does not name were read with `objdump`. REA was not attached. Static analysis did not observe execution. The sections above are unchanged.

### 1. `platformMetadata` suffix

**Observed.** The channel-service object stores two vtables. The primary vtable is `0x1484fab80`. The pointer at object `+0x8` is `0x1484fab88`. On that second vtable the slots are:

| Slot | Thunk | Target |
|---|---|---|
| `+0x60` | `FUN_140571454` (`0x140571454`) | `FUN_146439090` PERSONA getter |
| `+0x68` | `FUN_14057145c` (`0x14057145c`) | `FUN_146437fe0` OMNI getter |
| `+0x70` | `FUN_14057165c` (`0x14057165c`) | `FUN_146430df0` channel-key builder |

Each thunk is `mov rax, [rcx]; jmp [rax+slot]`. `RDX` and `R8` are left as the caller set them. `R8` is the suffix.

**Observed.** `FUN_145a886d0` (`0x145a886d0`) reads the lookup key through `FUN_140fca800` (`0x140fca800`) and the `+0x70` thunk. If that string length is 0 it logs `No OMNI lookup key to select metadata, using defaul id.` (`0x14844c040`) and builds the key with `FUN_1402b0e70` (`0x1402b0e70`):

- format `%s%d` at `0x147f58224`
- prefix `STEAM_APP_ID.` at `0x148044e18`
- the Steam app id from the Steam user virtual `+0x48`

The resulting suffix is the text `STEAM_APP_ID.<app id>`. `FUN_145a78d00` (`0x145a78d00`) then calls the `+0x68` thunk with that string as the third argument. The four strings copied back match the OMNI getter layout (`+0x00`, `+0x28`, `+0x50`, `+0x78`).

**Observed.** `FUN_145a911c0` (`0x145a911c0`, log `Initializing OmniSDK`) calls the same `+0x68` thunk with the string at object `+0x18`. It compares the returned stage string with `prod` (`0x14844aba0`).

**Observed.** `FUN_146439520` (`0x146439520`) writes the literal `STEAM_APP_ID.0000000.BRANCH.local-dev` (`0x1484fdbf8`, 0x25 bytes) into `product.id` on one branch, and `dev` on the other. The `.BRANCH` cut in `FUN_146437fe0` (first pass) stops at `.BRANCH`, so a lookup of `OMNI.STEAM_APP_ID.0000000.BRANCH.local-dev` retries as `OMNI.STEAM_APP_ID.0000000`.

**Inferred.** The document keys the auth manager looks up are `OMNI.<channel key>` and, when that key is empty, `OMNI.STEAM_APP_ID.<app id>`. The PERSONA getter is the sibling slot `+0x60`. This pass did not find a second suffix literal for it.

Resolved.

### 2. Order of `/steam` and the persona call

**Observed.** `FUN_145a7f3e0` (`0x145a7f3e0`) reads `client-connection.authorization.backend` (`0x14844b928`) into the int at `+0x2ec` (`0x145a7f9ba`, `FUN_140fc7f90`). It then calls `FUN_145a913e0` (`0x145a913e0`).

**Observed.** `FUN_145a913e0`: if `+0x2ec` is `0`, it copies the auth-stack endpoint into the client at `+0x200`. If `+0x2ec` is not `0`, it installs the other client through `FUN_145a7bec0` (`0x145a7bec0`). `FUN_145a7f3e0` calls `FUN_145a886d0` only when `+0x2ec` is not `0` (`0x145a7fa15`).

**Observed.** `FUN_145a886d0` reads a provider value. If it is not `2`, it reports `@mm_authresult_Error_Steam` (`0x14844bab8`). If it is `2`, it looks up OMNI metadata. When both returned string groups are non-empty it logs `Omni metadata is present for this channel, using OMNI Authentication.` (`0x14844c080`), constructs the persona client with `FUN_145a7ebb0` (`0x145a7ebb0`), stores it at `+0x228`, and writes `2` to `+0x2ec` (`0x145a88ab7`). When the lookup is empty it logs `Omni metadata is NOT present for this channel, Persona auth is not supported.` (`0x14844c0d0`) and does not store a new client.

**Observed.** `FUN_145a936f0` then uses that int, as the first pass said. `0` sends `/steam` and skips the persona block. Any other value, including `2`, takes the persona virtual calls and does not send `/steam` in that callback.

**Observed.** `CreateSession` (`FUN_1479b3cb0`) is still called only from `FUN_145a97310` (`0x145a97310`). That function has no direct `call`. Its address is stored at `0x14844ad98`. `FUN_145a911c0` calls `FUN_1479b0f60` (`0x1479b0f60`) and `FUN_1479b15f0` (`0x1479b15f0`) after the OMNI strings are present, and on success sets its own state to `2`.

**Inferred.** The client does not do `/steam`, then persona, then the Omni token call on one line. Backend `0` takes `/steam` in the ticket callback. Backend `2` with OMNI metadata takes the persona call in the ticket callback, after `FUN_145a886d0` has already stored the persona client. The Omni `CreateSession` function is not in the ticket callback.

Resolved.

### 3. `CharacterCreationParams` string

**Observed.** `FUN_1474ec2c0` still copies one string. No second function references the key `CharacterCreationParams` (`0x148703a28`). No format string concatenates fields into that JSON string.

**Observed.** A different writer, `FUN_146100140` (`0x146100140`, 363 bytes), serializes `Javelin.RPC.CharacterCreationParameters`. It is not called by `FUN_1474ec2c0`. Its fields are in section 9. That protobuf is not the JSON string.

**Unknown.** The bytes stored in the JSON string. No literal template and no concatenation were found.

Partially resolved.

### 4. Optional `Character` object

**Observed.** Constructors `FUN_1474e1000` (`0x1474e1000`), `FUN_146405fd0` (`0x146405fd0`), and `FUN_1464060e0` (`0x1464060e0`) set `Aws::JavelinGatewayService::Model::PatchCharacterRequest::vftable`. The referenced vtable address is `0x148703490`. `0x148703490 + 0xD0` is `0x148703560`, which holds `FUN_1474ecbc0`.

**Observed.** `FUN_146405fd0` and `FUN_1464060e0` copy the source object at `+0x188` and the presence flag at `+0x4f8` into the `PatchCharacterRequest`. Those are the same two offsets `FUN_1474ecbc0` tests and writes as the `Character` member.

**Inferred.** The optional `Character` object is the `Character` member of `PatchCharacterRequest`.

Resolved.

### 5. Character-select name join

**Observed.** `FUN_14641b020` is unchanged: a tile is a `Characters` record of `0x420` bytes whose world id matches. `OwningResourceId` (`0x148702f08`) is still read only by `FUN_1474e7090`, into name-record `+0x168`. No compare of that field with `CharacterId` was found. No function that copies `DisplayName` onto the `0x420` record was found.

**Inferred.** The select list does not require `DisplayName` or `OwningResourceId`. The script property `displayName` is separate from that filter.

Partially resolved.

### 6. Non-200 text for the four operations

**Observed.** Every direct caller of `FUN_146460240` was listed in this pass. They are `FUN_146413d60`, `FUN_14641b020`, `FUN_146425cb0`, `FUN_14642d950`, `FUN_14643d110`, `FUN_14643e4a0`, `FUN_14644a070`, and `FUN_1464708d0`. The getlogininfo, worlds/list, characters, and validator builders are not in that list. They store the HTTP status at `+0x80` and the operation id at `+0x84`, and they do not switch on the status.

**Observed.** Worlds list, in `FUN_14641b020`, uses fixed reasons and does not read the status:

| Condition | Reason | Log text from `FUN_141003620` |
|---|---|---|
| result pointer is null | `0x10` | `Errors::login_world_list_failed` |
| world range is empty | `0x11` | `Errors::login_no_worlds` |
| world fails the selectable check | `0x12` | `Errors::login_no_worlds_unknown` |

**Observed.** GetLoginInfo failure in `FUN_1464154e0`, when the status is `-1` and the message is empty, writes the literal `NoConnection`. It does not call `FUN_146460240`.

**Inferred.** Those four operations have no sibling of the queue status switch. The queue map in `FUN_14643d110` (first pass) is the only HTTP-status map that reaches the reason log.

Resolved.

### 7. CMS fetch failure

**Observed.** The full caller list of `FUN_146460240` has no CMS function. `OnWorldCMSDataSet` (`0x14831c1e0`) is still only an event name in `FUN_1441d5100` and `FUN_1441ee020`. `FUN_14644bc60` writes the three CMS cvars and returns without an HTTP result.

**Inferred.** A failed or missing CMS body does not call the login error reporter and does not block character select. The screen ignores it.

Resolved.

### 8. Persona client ready, and a missing `PERSONA.` entry

**Observed.** The persona client used by `FUN_145a936f0` is the pointer at `+0x228`. `FUN_145a886d0` replaces that pointer only when the OMNI metadata lookup returns non-empty strings. It then calls virtual `+0x8` on the new object (`0x145a88ab7` region, the call after the store). The ticket callback treats virtual `+0x10` returning zero as ready, and a null pointer as `Persona failed to initialize!`.

**Observed.** The miss path does not call `FUN_145a7ebb0` and logs that persona auth is not supported. `FUN_1479c4560` still selects `identity-service.amazongames.com` by stage. No function in this pass copies `personaHostV2` onto that host.

**Inferred.** A missing OMNI metadata entry does not create the persona client, so the persona branch of the ticket callback stops. The SDK host default is still the stage identity host, but the client pointer the ticket callback checks is null unless the lookup succeeded. Backend `0` does not take that branch.

Resolved.

### 9. Javelin.RPC protobuf writer

**Observed.** `FUN_146103200` (`0x146103200`, 326 bytes) writes `Javelin.RPC.GetLoginInfoRequest`. `FUN_146100140` (`0x146100140`, 363 bytes) writes `Javelin.RPC.CharacterCreationParameters`. Neither is larger than the skip limit.

**Observed.** The tag byte is `(field_number << 3) | wire_type`, then a base-128 varint. The continuation bit is `0x80`.

- `FUN_14609e170` (`0x14609e170`, 115 bytes) does `lea eax, [rcx*8]` and writes that tag, then the value in `DL`. Wire type is `0` (varint).
- `FUN_146099f10` (`0x146099f10`) does `lea r8d, [rdx*8]` and `or r8d, 2` at `0x146099f8c`. Wire type is `2` (length-delimited string). It calls `FUN_1478530b0` (`0x1478530b0`) first, and that function calls the UTF-8 check `FUN_147852e40`.

**Observed.** `GetLoginInfoRequest` fields in `FUN_146103200`:

| Field | Writer | Wire | Name |
|---|---|---|---|
| `1` | `FUN_1460cb7c0` (`0x1460cb7c0`) with `ecx = 1` | sub-message | not named in this function |
| `2` | `FUN_146099f10` with `edx = 2` | string | `channel_id` (`0x1484c1368`) |
| `3` | `FUN_14609e170` with `ecx = 3` | varint | byte at object `+0x20` |

**Observed.** `CharacterCreationParameters` fields in `FUN_146100140`:

| Field | Writer | Wire | Name |
|---|---|---|---|
| `1` | `FUN_146099f10` with field `1` | string | `name` (`0x1484c0e80`) |
| `2` | `FUN_1460cbb20` (`0x1460cbb20`) with field `2` | sub-message | not named in this function |
| `3` | `FUN_146099f10` with field `3` | string | not named in this function |
| `4` | `FUN_14609e170` with field `4` | varint | byte at object `+0x28` |

A trailing unknown-field blob is `memcpy`'d when bit 0 of the pointer at `+0x8` is set.

**Unknown.** The names of the unnamed fields. The tag rule and the field numbers above are the write loop.

Resolved.

## Negative searches (second pass)

Looked up and not found:

- A direct `call` to `FUN_146437fe0` or `FUN_146439090`.
- A suffix literal other than `STEAM_APP_ID.` plus the app id, the channel key, and `STEAM_APP_ID.0000000.BRANCH.local-dev`.
- A function that concatenates fields into the JSON `CharacterCreationParams` string.
- A compare of `OwningResourceId` with `CharacterId`.
- A direct caller of `FUN_146460240` in the getlogininfo, worlds/list, characters, or validator builders.
- A CMS function on the `FUN_146460240` caller list.
- A copy of `personaHostV2` onto the identity host.

## Unknowns (second pass)

1. The bytes inside the JSON `CharacterCreationParams` string (section 3).
2. The function that copies `NameReservations.DisplayName` onto a character tile, if one exists (section 5).
3. The names of protobuf fields 1 and 3 on `GetLoginInfoRequest`, and fields 2 and 3 on `CharacterCreationParameters` (section 9).

## Third pass: the two items left open

Date: 2026-09-02.
Method: ten review passes. Only one read-only Ghidra run (28 threads, 24 GB heap) wrote `<re-scratch>/out-open2`. REA `open_binary` was not used. Static analysis did not observe execution.

Labels: **Observed** = an address, a string, or decompiler output in this build. **Inferred** = a conclusion from that evidence. **Unknown** = not established.

Nine passes read older dumps and reported that `<re-scratch>/out-open2` was absent. Where a report disagrees with the Ghidra dump, this section follows the Ghidra dump.

### 1. `CharacterCreationParams`

Status: Still unknown.

**Observed.** `FUN_1474ec2c0` (`0x1474ec2c0`) copies an existing string. The key is `CharacterCreationParams`. The only direct caller is `FUN_1474ecf20` (`0x1474ecf20`) at `0x1474ecf6a`. That call is `FUN_1474ec2c0(param_1 + 0x1b0, ...)` when the byte at request `+0x250` is set. The string sits at CreateCharacter request `+0x1b0`. The presence flag is at `+0x1d0` (`0x1b0 + 0x20`). `FUN_1474ec2c0` writes the key when that flag is set. The write is `FUN_147465da0` (`0x147465da0`) on the string object at the start of the same struct. This function has no format string and no concatenation.

**Observed.** `FUN_147465da0` passes that string to `cJSON_CreateString`. One dump says the copy stops at the first NUL and does not use the C++ string size. Two dumps name different addresses for `cJSON_CreateString`: `0x1474967e0` and `0x1479e0800`. The Ghidra dump does not name that address, so this note does not choose one.

**Observed.** `FUN_1474ecf20` has no direct `call`. Its address is stored at `0x148703818`. A second data hit at `0x14ac71444` is not inside a function. The backward walk from those hits lands on `FUN_147ec8090` (`0x147ec8090`), a TLS callback. That function does not call the serializer.

**Observed.** The UI method `CreateCharacter` is `FUN_144ba2a00` (`0x144ba2a00`). Its only code xref in the Ghidra dump is `0x144f478d8` in `FUN_144f40780` (`0x144f40780`). It builds four records of `0x60` bytes and calls `FUN_144ba2ac0` (`0x144ba2ac0`). `FUN_144ba2ac0` applies four script argument groups. It does not call the JSON writer and it does not concatenate `CharacterCreationParams`. Older passes said this body was not in the dumps. This section follows the Ghidra dump.

**Observed.** `FUN_146100140` (`0x146100140`) writes protobuf `Javelin.RPC.CharacterCreationParameters`. It does not call `FUN_1474ec2c0`. `FUN_14609e170` (`0x14609e170`) writes a protobuf tag (`field_number * 8`, then one value byte). `FUN_146100140` uses it for field 4. `FUN_1460cbb20` (`0x1460cbb20`) writes a length-delimited protobuf tag (`field_number * 8 | 2`). `FUN_146100140` uses it for field 2. `FUN_1460e3490` (`0x1460e3490`) writes only the type name `Javelin.RPC.CharacterCreationParameters` (39 bytes). It writes no field values. Older passes said the bodies of `FUN_14609e170`, `FUN_1460cbb20`, and `FUN_1460e3490` were not in the dumps. This section follows the Ghidra dump.

**Observed.** No dumped function stores the string at request `+0x1b0` or sets the byte at `+0x1d0`.

**Inferred.** Ruled out: appearance JSON keys in the dumps (`Gender`, `Hair`, `Facial`, `BodyType`, `VoicePitch`, `eyeColor`, `skinTone`, and the appearance, customization, gender, and hair names in the string tables). Ruled out: protobuf bytes from `FUN_146100140` as this field. Ruled out: the UI `CreateCharacter` method as the builder.

**Unknown.** The function that writes the string at CreateCharacter request `+0x1b0` and sets the byte at `+0x1d0`.

### 2. Character tile name

Status: Resolved.

**Observed.** `FUN_1464278c0` (`0x1464278c0`) builds each `0x420` tile from one `Characters` record. The source stride is `0x370`. `FUN_1464154e0` (`0x1464154e0`), the GetLoginInfo handler, calls `FUN_1464278c0` and `FUN_146428750` (`0x146428750`) as separate copies.

**Observed.** `FUN_1474e1f60` (`0x1474e1f60`) stores JSON `Name` at character-record `+0xf8` and `WorldId` at `+0x348`. `CharacterId` is the string at `+0x00`. `FUN_141612310` (`0x141612310`) is the tile constructor. It stores `Characters.Name` (source `+0xf8`) at tile `+0x78`, `WorldId` (source `+0x348`) at tile `+0x50`, and `CharacterId` at tile `+0x00`. The constructor has no reference to `DisplayName` or `OwningResourceId`. A second caller, `FUN_1417423e0` (`0x1417423e0`), compares the text `ACTIVE`. That path is world status, not the character-list copy. Older passes inferred the tile slots because `FUN_141612310` was not dumped. The Ghidra dump decompiled the constructor. This section follows that dump.

**Observed.** `FUN_14641b020` (`0x14641b020`) walks tiles of `0x420` bytes. It keeps a tile when `memcmp` says the string at tile `+0x50` matches the selected world id. It then `_stricmp`s the string at tile `+0x78` with the player name at `GameConnection + 0x1050`. The direct caller is the unlabeled thunk at `0x146475540`:

```
mov rdx, qword ptr [rdx]
add rcx, 8
jmp 0x14641b020
```

That address is stored at `0x148502968`. The thunk has no direct `call`. `FUN_146475490` (`0x146475490`) ends at `0x146475510` and is not the filter. The thunk does not store `DisplayName` or `OwningResourceId`. Older passes did not name this caller. This section follows the Ghidra dump.

**Observed.** `FUN_1474e7090` (`0x1474e7090`) reads one `NameReservations` element. The Ghidra dump stores `DisplayName` at name-record `+0x78` and `OwningResourceId` at `+0x168`. The same function, in the older dumps, also stores `NormalizedName` at name-record `+0x118`. The Ghidra dump does not mention `NormalizedName`. The offsets it does name match, so both stores stand. `FUN_146428750` copies `DisplayName` (source `+0x78`) and `OwningResourceId` (source `+0x168`) into a separate vector. Each output element is `0x170` bytes, through `FUN_1463fd9e0` (`0x1463fd9e0`). The source stride is `0x258`. This copy does not write the `0x420` tile. The older dumps that name this function say it does not copy `NormalizedName`. `FUN_141612310` and the thunk at `0x146475540` do not copy `DisplayName` or `OwningResourceId` onto the tile. No compare of `OwningResourceId` with `CharacterId` was found.

**Observed.** `FUN_144f40780` (`0x144f40780`) binds `displayName` (`0x148156be8`) and `displayNamePriority` (`0x1483bc678`) on `StaticLeaderboardData` (`0x1483b0ff8`). The same function binds `displayName` again on `StaticMountData` (`0x1483b10a0`). `CharacterMetadata` in that function does not bind either name. These binds are not the character tile.

**Inferred.** The name on the tile is `Characters.Name`. There is no join through `OwningResourceId`.

## Unknowns (third pass)

1. The writer of CreateCharacter request `+0x1b0`.

## Fourth pass

Status: Decompiler cannot resolve.

**Observed.** `FUN_14642b130` (`0x14642b130`) stores a string at CreateCharacter request `+0x1b0`. It sets `+0x1d0` and `+0x250` to 1. The string is base64 from `FUN_14634ffb0` (`0x14634ffb0`). The input of that encode is the output of `FUN_1413d4240` (`0x1413d4240`) selector 4, which calls `FUN_147aa01f0` (`0x147aa01f0`).

**Observed.** `FUN_147aa01f0` does not parse JSON and does not build JSON. It writes `stream error`, `buffer error`, and the bytes `0x1f`, `0x8b`, `0x08`. It returns `0xfffffffe` and `0xfffffffb`. Selector 4 is the only value accepted when the state is `0x29a`.

**Inferred.** `FUN_147aa01f0` is zlib `deflate`. Selector 4 is `Z_FINISH`. The stored text is base64 of gzip data, not raw JSON.

**Observed.** `FUN_14642b130` has no direct call. Data references are `0x1484fdf50` and `0x14abcae9c`. The buffer at `param_4 + 0x20` is an incoming argument. This function has no key literals (`currency`, `visualsBlob`, `position`, `x`, `y`, `z`, `currentLevel`, `worldId`, or any other key).

**Unknown.** The function that writes `param_4 + 0x20`. The search stops at `FUN_14642b130` (`0x14642b130`). A decompiler of that function cannot see the key characters.

## Fifth pass

Status: The name table and the `/steam` test are resolved. The int stored at object `+0x2ec` when both overrides are absent is a static limit.

Method: static only, on the existing Ghidra project, read-only, analysis off (`analyzeHeadless -process NewWorld.exe -noanalysis -readOnly`). REA `binary_session` with `{}` reported `open: false`. `open_binary` was not called. The game was not run. Easy Anti-Cheat was not analyzed. Dumps are under `<re-scratch>/out-fifth/`.

### 1. Default of `client-connection.authorization.backend`

**Observed.** The literal default index is `2`. `FUN_146168bc0` (`0x146168bc0`) passes it with the cvar name and the 3-name table:

```
0x146168f7d  MOV dword ptr [RSP+0x20], 3
0x146168f85  LEA R9, [0x149f3fd58]
0x146168f8c  MOV R8D, 2
0x146168f96  CALL 0x140571670
```

`RCX` at that call is `MOV RCX, qword ptr [RBX+0x28]` (`0x146168f71`). `RBX` is the current provider node. The same `2` is passed again at `0x1461690ef`. `0x140571670` is `MOV RAX, [RCX]` / `JMP [RAX+0x90]`.

**Observed.** Index `2` in the table at `0x149f3fd58` is the string `omni` (`0x1484c7fa8`). `FUN_14618e270` (`0x14618e270`) passes the same default `2` at `0x14618e345` into `FUN_1461a91e0` (`0x1461a91e0`) for the key `authorization.backend` (`0x1484c9668`).

**Observed.** A launch argument is not required for that literal to exist. `FUN_14645b520` (`0x14645b520`) calls `FUN_1461a9950` (`0x1461a9950`) only when the `AuthBackend` value length is not zero. A JSON key is not required for the literal either. `FUN_1461a91e0` calls the setter `FUN_14613dc40` (`0x14613dc40`) only after `FUN_1461a54f0` finds `authorization.backend`. If that lookup fails, it jumps to `0x1461a93d8` and does not call the setter.

**Observed.** `FUN_140fc7f90` (`0x140fc7f90`) writes the getter result to the out pointer. It does not write that pointer when both provider lists are empty. The context from `FUN_1406d17b0` (`0x1406d17b0`) is `0xa8` bytes, vtable `0x147f20210`. The constructor stores `0` in the list at `+0x58` (count at `+0x90`) and in the list pointer at `+0x20`. `FUN_145a7f3e0` (`0x145a7f3e0`) does not store `[RDI+0x2ec]` before the call at `0x145a7f9ba`. `FUN_145a88640` (`0x145a88640`) allocates the object with `malloc` (`0xda8` bytes) and does not clear it.

**Unknown. Static limit.** The image does not show a call that enters `FUN_146168bc0`. Its only reference is vtable `0x1484c9618` slot `+0x08` at `0x1484c9620`. `FUN_1461521d0` (`0x1461521d0`) writes that vtable. `FUN_14643f0a0` (`0x14643f0a0`) stores the object at parent `+0x240` (`0x14643f334`). `FUN_14102ee20` (`0x14102ee20`) does the same (`0x14102f11a`). Those two creators have no direct callers. They are slots on parent vtables `0x1484faad0` (`+0x590`) and `0x147fc6118` (`+0x138`). No method of either parent loads `[reg+0x240]` and passes that pointer to a call. Eight same-function calls of slot `+0x08` after a `[reg+0x240]` load are refcount destructors (`LOCK XADD [object+0x08]`), at `0x14373a608`, `0x1441d93d8`, `0x1447a0e41`, `0x1447d9f11`, `0x14499f2b3`, `0x1449a1a89`, `0x14672d1d5`, and `0x14673323b`. Forty-five direct callees that receive the pointer do not call slot `+0x08`. Thunk `0x140571444` has 34 call sites. None of those functions loads `[reg+0x240]` or writes vtable `0x1484c9618`. The concrete function at provider slot `+0x90` was not named, because the qword at node `+0x28` is not an immediate and no store of a vtable into that field was found in the cvar-context callers. The int written to object `+0x2ec` when `-AuthBackend` is absent and `ClientOverride.json` has no `authorization.backend` key is not established. If both provider lists stay empty, `FUN_140fc7f90` leaves the dword as leftover `malloc` bytes.

### 2. How `AuthBackend` text becomes the int

**Observed.** The text is a name. It is not a decimal. The build has the string `AuthBackend` at `0x1484fdd10`. It does not have the literal `-AuthBackend`. `FUN_14645b520` loads that string at `0x14645b9c1`, sets `EDX` to `1`, and calls virtual `+0x18`. When the value length is not zero it calls `FUN_1461a9950` at `0x14645ba05`.

**Observed.** `FUN_1461a9950` calls `FUN_1461622b0` (`0x1461622b0`) at `0x1461a9975`. `R8D` is the count dword `3` at `0x149f40094`. `RDX` is the table at `0x149f3fd58`. The compare is a full-length `memcmp` at `0x146162322`. The lengths must be equal. The match is case-sensitive. On a match, `MOV EAX, EDI` at `0x14616233a` returns the slot index. If no slot matches, the function returns the count, `3`. There is no `atoi`, `strtol`, or `sscanf` on this path. A decimal string such as `0` misses the names, so the value passed onward is `3`.

| Text | Int | String | Table slot |
|---|---|---|---|
| `nw` | `0` | `0x1484c7fa4` | `0x149f3fd58` |
| `persona` | `1` | `0x14844b8b0` | `0x149f3fd60` |
| `omni` | `2` | `0x1484c7fa8` | `0x149f3fd68` |
| any other command-line text | `3` | none | past the last slot |

**Observed.** `FUN_1461a9950` passes that index in `R9D` at `0x1461a9a63` and calls `0x140571500` at `0x1461a9a71`. That thunk is `MOV RAX, [RCX]` / `JMP [RAX+0x180]`. `RCX` is again `[RBX+0x28]`.

**Observed.** `ClientOverride.json` uses the same three names through `FUN_1461a91e0`, with `_stricmp`, so `NW`, `Persona`, and `OMNI` match. The call from `FUN_14618e270` passes default index `2`. A key that is present and does not match a name keeps index `2`. A key that is absent does not call the setter.

**Observed.** These texts are not in this table. A NUL-terminated search of this build:

| Text | Where it is used |
|---|---|
| `authstack`, `AuthStack`, `AUTHSTACK` | no hit |
| `authStack` (`0x1485013b0`) | channel tag in `FUN_146450250` only |
| `steam` (`0x147f48c88`) | platform enum pointer at `0x14a12a090` |
| `mock` (`0x147fc8184`) | platform enum pointer at `0x14a12a0f8` |
| `sonic` (`0x147fc815c`) | `FUN_14643f0a0`, `FUN_14102ee20` |
| `scarlett` (`0x147fc8168`) | `FUN_14102ee20`, `FUN_145f21890` |
| `prospero` (`0x147fc8178`) | `FUN_14102ee20`, `FUN_145f21890` |

**Unknown. Static limit.** The body of provider slot `+0x180` was not named. The same missing vtable at node `+0x28` blocks it. The index is passed in `R9D`. The store into the container that `FUN_140fc7f90` later reads was not shown as an instruction.

### 3. Which int takes `/steam`, and what else can force `+0x2ec` to `0`

**Observed.** Only the 32-bit value `0` enters the `/steam` arm of `FUN_145a936f0` (`0x145a936f0`). `0x145a9371a` is `MOV RBX, RCX`, and `RBX` is still the object at the compare:

```
0x145a93cbc  CMP dword ptr [RBX+0x2ec], 0
0x145a93cc3  JNZ 0x145a9418a
```

The value `0` falls through. Every other 32-bit value, including `1` and `2`, jumps to `0x145a9418a`. This function has no other compare of `+0x2ec`. The `/steam` bytes are only in the zero arm: `0x145a93e29` loads `0x14844bbb8` (`2f 73 74 65 61 6d 00`, `/steam`) and `0x145a93e3d` sets the length to `6`. A second test in that arm does not read `+0x2ec`. `0x145a93d3e` compares `[R14+0x10]` with `0`, and `0x145a93d43` jumps to the copy only when that qword is not `0`. A zero backend with endpoint length `0` does not copy `/steam`.

**Observed.** On the Steam auth manager, the stores of this dword are:

| Site | Value | When it runs |
|---|---|---|
| `FUN_140fc7f90` at `0x140fc80c1` | `EAX` from the getter | first provider loop calls thunk `0x140571548` (`JMP [RAX+0x1d0]`) |
| `FUN_140fc7f90` at `0x140fc826c` | `EAX` from the getter | second provider loop, the list at provider `+0x20` |
| `FUN_145a886d0` at `0x145a88ab7` | constant `2` | both OMNI metadata string lengths are non-zero |

`FUN_145a7f3e0` calls the getter at `0x145a7f9ba` with `RCX = object+0x2ec` and the name `client-connection.authorization.backend` (`0x14844b928`). The empty OMNI-metadata path logs `Omni metadata is NOT present for this channel, Persona auth is not supported.` and does not write the int. The provider-not-`2` path reports `@mm_authresult_Error_Steam` and does not write the int. `FUN_145a913e0`, `FUN_145a936f0`, `FUN_145a8c7c0`, `FUN_145a8e8d0`, `FUN_145a825d0`, `FUN_145aa2bf0`, and `FUN_145a8d1a0` only read the dword. The base constructor `FUN_145a7e910` stops at offset `0x178`. The derived constructor writes a byte at `+0x2e8` and a dword at `+0x2f0`. Those accesses do not cover `0x2ec` through `0x2ef`.

**Observed.** No other cvar name is passed with this address. No channel-document store writes `0` to this field. OMNI metadata can replace the value with `2`. It cannot replace the value with `0`. The only writer that can produce `0` is the getter for `client-connection.authorization.backend`, and only when that getter returns `0`.

**Observed.** `FUN_145a7f1d0` at `0x145a7f353` writes a qword at `+0x2e8` on a different object, the Sonic auth manager (size `0x360`, vtable `0x14844a418`). The stored value is an allocator pointer, not this backend int.

**Unknown. Static limit.** The body of getter slot `+0x1d0` was not named. The thunk is `0x140571548`. The call sites are `0x140fc80be` and `0x140fc826a`. The object is the same unnamed pointer at provider node `+0x28`. This pass does not show the int that slot returns for stored index `2`.

Evidence: `<re-scratch>/out-fifth/q1-default/`, `q1-getter/`, `q2-parser/`, `q2-table/`, `q3-steam/`, `q3-writers/`.
