# The shop and entitlement services

Date: 2026-11-02. Build: Steam `25810808` (`NewWorld.exe` MD5 `ac8791b8…`). Labels: **Observed**
(seen in code, data, a log or a capture), **Inferred** (concluded from observed facts), **Unknown**
(not established).

## Summary for a non-programmer

- After login our client called Amazon's shop service once (`2026-11-02-region-ping.md`,
  section 3).
- The client takes the shop addresses from three remote config keys. Retail leaves them empty,
  so the client used its built-in Amazon addresses.
- Now our remote config fills the keys with our game host, the same way it fills the
  leaderboard address, and our server answers the shop calls: no items for sale, no
  entitlements.

## 1. Evidence

### 1.1 The client code

- **Observed.** `FUN_1416190f0` (Ghidra sweep `local-data/re-sweep`) makes the
  `FcsEndpointClient`. It reads `omni.fcs.assume-role`, `omni.fcs.region`,
  `omni.fcs.endpoints.catalog-service`, `omni.fcs.endpoints.entitlement-service` and
  `omni.fcs.endpoints.transaction-service` with `FUN_140fcb290`, and gives the region and the
  catalog endpoint to `FUN_1417230a0`, which builds the client configuration
  (`Aws::Catalog::CatalogClient`). The leaderboard client `FUN_141627230` reads
  `omni.leaderboards.*` the same way, and our remote config already sets
  `omni.leaderboards.client-endpoint` to `game.<domain>` (`2026-09-18-leaderboards-and-analytics.md`).
- **Inferred.** With an empty endpoint key the client uses the SDK's built-in host:
  `client.catalogservice.amazongames.com`, `client.entitlementservice.amazongames.com` or
  `client.transactionservice.amazongames.com`.

### 1.2 Retail remote config and our live run

- **Observed.** The retail remote config capture (`local-data/remote-config/retail-2026-09-18`)
  has no `fcs` key.
- **Observed (live run 2026-11-02).** For about 15 s after login our client held a connection to
  `34.195.204.23:443`, one of the two addresses of both the catalog and the entitlement host.

### 1.3 Retail calls and replies

From the retail captures under `local-data/captures` (folders `catalog` and `entitlements`):

| Call | Reply |
|---|---|
| `GET /players/<persona>/games/new-world/platforms/steam/catalog?type=MTX` | `200` JSON: `currencyCode`, `offers` (365), `countryCode`, `etag`; or `304` |
| `GET .../stores/<alias>` (`Featured`, `Seasonal`, `Special1`, `Special2`) | `200` JSON: `currencyCode`, `stores` (one store: `alias`, `metadata`, `nextScheduleStartDate`, `slots`), `countryCode`, `etag`; or `304` |
| `GET .../entitlements` | `200` JSON: `hasMoreResults`, `entitlements` (100 a page), `paginationToken` |
| `POST .../entitlements/sync` with `{"syncTypes": [5 names]}` | `202` with no body |

- **Observed.** The catalog and store replies carry the same value in the `ETag` header (no
  quotes) and in the `etag` member: 44 base64 characters. A `304` has
  `Content-Type: text/plain; charset=utf-8`. The JSON replies have
  `Content-Type: application/json`.
- **Observed.** The capture of the privacy settings service (`POST .../privacy-settings`) is a
  separate host, read from `omni.privacy-settings.client-endpoint` (`FUN_14160ad60`). Our live
  run showed no connection to it.

## 2. What the server does

- `crates/nw-login/src/remote_config.rs` fills `GAME_HOST_ENDPOINTS` of the publicGameplay
  `RegionId` layer with `game.<domain>`: the leaderboard key and the three `omni.fcs.endpoints.*`
  keys. The tracked layer has `omni.fcs.region` `us-east-1` and an empty `omni.fcs.endpoints`
  object; the server refuses a file that sets one of the keys itself.
- `crates/nw-login/src/shop.rs` serves on `game.<domain>`: an empty catalog, no store for any
  alias, an empty entitlement list, and `202` for a sync. The catalog and store replies have an
  ETag (base64 of the SHA-256 of the reply with an empty `etag`) and answer `304` to it.
- **Inferred.** `currencyCode` `USD` and `countryCode` `US`: retail answers in the player's own
  currency and country, and this server has one region.
- **Inferred.** The last entitlement page leaves out `paginationToken`; no retail last page was
  captured.
- The purchase service (`transaction-service`) points at `game.<domain>` too, so a purchase never
  reaches Amazon. This server has no purchase route; it answers `404`.

## 3. Live check (Linux, 2026-11-02)

- **Observed.** After a new game start, at character select, the game held connections only to
  our server (`127.0.0.1:443`, `127.0.0.1:80`) and to a local port, for 40 s. edge-http counted
  one `202`, the reply only an entitlement sync gets.

## 4. Unknown

1. How the shop screen shows an empty catalog and no stores. A live run with the shop screen
   open answers it.
2. Whether our client calls the privacy settings service in some screen. Its endpoint key
   (`omni.privacy-settings.client-endpoint`) is not set yet.
