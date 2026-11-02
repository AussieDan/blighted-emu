# The news hosts: message of the day, news stories, menu tiles and their images

Date: 2026-11-02. Build: Steam `25810808` (`NewWorld.exe` MD5 `ac8791b8…`). Labels: **Observed**
(seen in code, data, a log or a capture), **Inferred** (concluded from observed facts), **Unknown**
(not established).

## Summary for a non-programmer

- Our patched client called two Amazon hosts on the real internet while we played: the news host
  `d1hkbwzm1bktgo.cloudfront.net` and its image host `client.content-service.amazongames.com`. It
  asked for the message of the day once a minute.
- Our launcher did not send these names to our server, so the calls left the PC.
- Now our server answers both names with its own news files (`config/cms/`), and the launcher
  sends both names to our server: on Linux with the private hosts file, on Windows with the
  loopback proxy and a fourth client patch.

## 1. Evidence

### 1.1 Our client logs

- **Observed.** `Game.log` of our launcher's prefix (run of 2026-11-01) has 24 lines
  `[CMS] Loading CMS HTTP Request URL: https://d1hkbwzm1bktgo.cloudfront.net/...` and 6 lines for
  `https://client.content-service.amazongames.com/images/...`. The message of the day
  (`/motd/worlds_STEAM_APP_ID.1063730.json`) repeats every 60 seconds during play.
- **Observed.** Over that log, its 54 `LogBackups` and our saved run logs: 2066 lines name the
  news host and 336 name the image host. No log line names another Amazon host.
- **Observed.** The three documents are `motd/worlds_STEAM_APP_ID.1063730.json`,
  `newsstories/STEAM_APP_ID.1063730/metadata.json` and
  `marketingtiles/STEAM_APP_ID.1063730/metadata.json`. They match the cvar formats
  `worlds_%s.json`, `newsstories/%s/metadata.json` and `marketingtiles/%s/metadata.json`
  (`2026-09-02-login-unknowns.md`, section 10).

### 1.2 The fetch function

- **Observed.** `FUN_1430089b0` (callers `0x142fe7da0`, `0x143006880`; Ghidra sweep
  `local-data/re-sweep`) logs `Loading CMS HTTP Request URL: %s`, builds a default
  `Aws::Client::ClientConfiguration` (`FUN_147467a40`), makes an HTTP client from it
  (`FUN_14747f190`) and a GET request (`FUN_14747f250`). It sends `If-None-Match` with the ETag it
  kept, and on status 200 keeps the new `ETag` header and reads `content-length` bytes of the
  body. The same function fetches the documents and the images.
- **Observed.** It sets no proxy, no endpoint and no CA. So it is a fourth AWS SDK client of the
  same kind as the S3, Kinesis and STS clients (`2026-09-20-windows-launcher-network.md`).
- **Observed.** Its constructor call is at VA `0x143008A0B`: `LEA RCX,[RBP-0x10]` / `CALL` /
  `NOP` / `MOV byte ptr [RBP+0x4C],0` / `LEA RDX,[RBP-0x10]`. `scripts/patch-client.py` finds it
  by these bytes (`news proxy` in `AWS_CONFIG_SITES`); the plan on our `NewWorld.exe` puts the
  jump at file offset `0x3008010` and its cave at `0x1F8B0`.
- **Inferred.** With no change, the client checks the server certificate with the system trust
  store, as the S3 client does. On Linux the launcher's private trust file holds the mirror
  certificate, so no patch is needed there.

### 1.3 Retail replies

From the retail captures under `local-data/captures/open-world/*/content_motd` and
`content_service` (calls of the retail client to the real hosts):

- **Observed.** The news host answers `200` or `304 Not Modified` with `Server: AmazonS3`,
  `Content-Type: binary/octet-stream` and an `ETag` of 32 hex digits in quotes. It is S3 behind
  CloudFront.
- **Observed.** The image host answers with `Server: Contentful Images API`,
  `Content-Type: text/plain; charset=utf-8`, an `ETag` and `Cache-Control: max-age=31536000`. The
  body is a DDS image (starts with `DDS `).
- **Observed.** The message of the day has three objects: `worlds` (world id to `announcement`
  and `warning`, each also with the prefixes `es-ES-`, `es-MX-`, `fr-FR-`, `de-DE-`, `pl-PL-`,
  `it-IT-`, `pt-BR-`), `globalAnnouncement` (`title` and `description`, localized the same way)
  and `worldSets` (world set id to `setName`).
- **Observed.** `newsStories` is an array. A story has `backgroundImage`, `buttonText`,
  `content`, `convertToLocalTimeZone`, `internalName`, `popUpOnReturnToMenu`, `popupCount`,
  `popupMode`, `tabTitle`, `timeDisplayMode` and `title`, each also localized.
- **Observed.** `tileData` is an array. A tile has `description`, `imgKey`, `internalName`,
  `isVideoUrl`, `title` and `urlToOpen`, each also localized. `backgroundImage` and `imgKey` are
  URLs on the image host.

## 2. What the server does

- `nw_protocol::aws_mirror::is_cms_host` accepts exactly the two hosts. `is_aws_mirror_host`
  accepts them, so the launcher may map them (hosts file on Linux, `CONNECT` on Windows).
- `config/edge.toml` lists both hosts in `[aws_mirror] hosts`. The server refuses to start
  without them. The mirror certificate must be made again after the change of the list.
- `[cms] directory` (`config/cms/`) holds one folder per host, named after it. The news host
  folder holds exactly the three documents, each with its retail top-level members. Every image
  URL on the image host that a document names needs its file in the image host folder.
- `crates/nw-login/src/cms.rs` serves both folders as S3 objects (`crates/nw-login/src/s3.rs`,
  shared with the remote config sets): the body with its MD5 `ETag`, `304` for a matching
  `If-None-Match`, and the S3 `NoSuchKey` reply for any other key.
- The tracked documents have no news: an empty `newsStories` and `tileData`, empty announcements
  for the world of `config/edge.toml`, and the world set `live` (our `WorldSet`) with the set name
  `Blighted`.
- **Inferred.** The image host is answered with `binary/octet-stream`, not the retail
  `text/plain`. The client reads the body by `content-length` and does not check the type
  (section 1.2).

## 3. Other retail hosts

- **Observed.** The retail captures also show calls to `client.catalogservice.amazongames.com`,
  `client.entitlementservice.amazongames.com`,
  `client.agsprivacysettingsservice.amazongames.com`, DynamoDB in five regions, the gateway host
  `d1w0bfy6smo4d1.cloudfront.net`, `prod.identity-service.amazongames.com`,
  `tokenservice.amazongames.com` and Vivox.
- **Observed.** No log of our client names one of them.
- **Observed.** The retail DynamoDB calls are `GET /ping` on port 80 (plain HTTP), 22 for each
  of the five regions in one capture; the reply is 42 bytes of `text/plain`.
- **Observed (live run 2026-11-02, after the news reroute).** At character select our client
  held one TCP connection to `127.0.0.1:443` (our server) and one to `3.218.181.159:80`. That
  address is in the same `/24` block as `dynamodb.us-east-1.amazonaws.com` (`3.218.181.10` at
  that time). edge-http counted `304` replies that only the client's news polls send.
- **Inferred.** Our client still sent the DynamoDB region pings to Amazon over port 80. Fixed
  the same day: `2026-11-02-region-ping.md`.
- **Unknown.** Whether our client calls the other hosts of this list. A network capture of one
  play session under our launcher answers it.

## 4. Unknown

1. How the client shows empty `newsStories` and `tileData` lists. A live run answers it.
2. Where the client shows `worldSets[].setName`.
3. Whether the news and image calls reach our server in a live run on Linux and on Windows (the
   check: the edge-http log shows `GET` requests for the two hosts, and `Game.log` shows no
   error for them).
