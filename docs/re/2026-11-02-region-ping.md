# The region ping: `GET /ping` on DynamoDB, port 80

Date: 2026-11-02. Build: Steam `25810808` (`NewWorld.exe` MD5 `ac8791b8…`). Labels: **Observed**
(seen in code, data, a log or a capture), **Inferred** (concluded from observed facts), **Unknown**
(not established).

## Summary for a non-programmer

- The client measures its ping to each server region. For that it calls an Amazon database host,
  `dynamodb.<region>.amazonaws.com`, over plain HTTP on port 80.
- After the news fix, our client still did this toward Amazon.
- Now our server answers the ping on its own port 80, and the launcher sends the ping host to our
  server: on Linux with the private hosts file, on Windows with the loopback proxy and a fifth
  client patch. The ping now measures the way to our server.

## 1. Evidence

### 1.1 Retail captures

- **Observed.** The retail captures under `local-data/captures` (folders `dynamodb`) hold
  `GET /ping` on port 80 to `dynamodb.<region>.amazonaws.com` for five regions: `us-east-1`,
  `us-west-2`, `eu-central-1`, `sa-east-1` and `ap-southeast-2`; 22 for each region in one
  capture.
- **Observed.** The reply is `200` with `Content-Type: text/plain`, `Server: Server`, an
  `x-amzn-RequestId` and `x-amz-crc32`. The body is `healthy: <host> ` with one space at the end
  (42 bytes for `us-east-1`, 47 for `ap-southeast-2`).
- **Observed.** The five regions are the regions of the five `*-prod` clusters of the retail
  channel document (`2026-09-02-post-registration-sequence.md`, section 4).

### 1.2 The client code

- **Observed.** `FUN_14642ce60` (Ghidra sweep `local-data/re-sweep`) makes the
  `AwsRegionPinger` with its `awsRegionPingType` (`static` or `random`) and calls
  `FUN_146402310(object, config, 0)`.
- **Observed.** `FUN_146402310` builds a default `Aws::Client::ClientConfiguration`
  (`FUN_147467a40`), then writes its scheme (`+0x20`) as `param_3 != 0`, so `0`, plain HTTP, a
  `DefaultRetryStrategy`, and `verifySSL = 1` (`+0x1B8`). It makes its HTTP client with
  `FUN_14747f190`. So it is an AWS SDK client of the same kind as the four others
  (`2026-09-20-windows-launcher-network.md`).
- **Observed.** `FUN_1474bafd0` builds the host: `dynamodb.`, an optional `dualstack.`, the
  region, `.amazonaws.com`. `FUN_146454af0` calls it for each region of its list, and the ping
  thread `FUN_1464551b0` pings the hosts in turn.
- **Observed.** The constructor call is at VA `0x146402356`: `LEA RCX,[RSP+0x30]` / `CALL` /
  `NOP` / `MOV EAX,EBP` / `CMP byte ptr [RSI+0x1E8],AL` / `SETNZ AL`. `scripts/patch-client.py`
  finds it by these bytes (`region ping proxy` in `AWS_CONFIG_SITES`). The jump replaces the NOP
  and both short instructions; the cave replays them, and the flags of the `CMP` reach the
  `SETNZ`, because a jump does not change flags. The plan on our `NewWorld.exe` puts the jump at
  file offset `0x640195B` and its cave at `0x1F900`.
- **Observed.** The WinHTTP client (`FUN_147491570`) builds its proxy string from the scheme of
  the config (`+0x20`), `=`, the proxy scheme, `://`, the proxy host, `:` and the proxy port. For
  this client it is `HTTP=http://127.0.0.1:47443`. **Inferred** (WinHTTP named proxy): WinHTTP then
  sends each ping to the proxy as `GET http://dynamodb.<region>.amazonaws.com/ping`, not as a
  `CONNECT`.

### 1.3 Our client

- **Observed (live run 2026-11-02).** At character select our client held a TCP connection to
  `3.218.181.159:80`, in the same `/24` block as `dynamodb.us-east-1.amazonaws.com`
  (`2026-11-02-cms-news-hosts.md`, section 3).
- **Observed.** Our channel document names one region, `us-east-1` (`API_REGION` in
  `crates/nw-login/src/game.rs`).
- **Inferred.** The client pings the region of each cluster in the channel document, so our
  client pings only `dynamodb.us-east-1.amazonaws.com`.

## 2. What the server and the launcher do

- `nw_protocol::aws_mirror::is_dynamodb_host` accepts `dynamodb.<region>.amazonaws.com`, and
  `is_aws_mirror_host` accepts it, so the launcher may map it.
- `config/edge.toml` lists `dynamodb.us-east-1.amazonaws.com` in `[aws_mirror] hosts`. The server
  refuses to start without the ping host of `API_REGION`. The mirror certificate must be made
  again after the change of the list.
- `ping_listen` (`0.0.0.0:80`) is a plain HTTP listener. `crates/nw-login/src/region_ping.rs`
  answers `GET /ping` on a DynamoDB host with the retail body, `text/plain` and `x-amz-crc32`;
  another host gets `421`, another request `404`. The bot stack (`scripts/live-stack.sh`) moves it
  to a high port, because the bots send no ping.
- Linux: the private hosts file sends the ping host to the server, and the client reaches port 80.
- Windows: `--aws-proxy` gives the pinger's client a cave too. The launcher proxy accepts
  `GET http://<DynamoDB host>/...` (and no other plain request), opens a TCP connection to the
  server's port 80 and passes the bytes unchanged both ways.
- **Inferred.** The server answers with `Server` and `x-amzn-RequestId` left out. The client reads
  the status and the time the reply took.

## 3. Live check (Linux, 2026-11-02)

- **Observed.** The launcher's hosts file for the game names `dynamodb.us-east-1.amazonaws.com`.
  At character select the game held connections to `127.0.0.1:443` and `127.0.0.1:80` (our
  server), and to no DynamoDB address. Our reply matches retail byte for byte (body and
  `x-amz-crc32: 3128867991`).
- **Observed.** For about 15 s after login the game also held a connection to
  `34.195.204.23:443`. That address is one of the two addresses of both
  `client.catalogservice.amazongames.com` and `client.entitlementservice.amazongames.com`.
- **Observed.** The client reads these endpoints from the keys `omni.fcs.endpoints.catalog-service`
  and `omni.fcs.endpoints.entitlement-service` (`FUN_1416190f0`). Neither our remote config sets
  nor the retail capture of the remote config (`local-data/remote-config/retail-2026-09-18`) set
  them.
- **Inferred.** Our client still called Amazon's store or entitlement service once after login.
  Fixed the same day: `2026-11-02-shop-services.md`.

## 4. Unknown

1. Whether the client shows the ping to our server in the world list. A live run answers it.
2. Whether `awsRegionPingType` `random` pings other regions than those of the channel document.
3. A live test on Windows (`docs/client-patch.md`, Windows live test).
