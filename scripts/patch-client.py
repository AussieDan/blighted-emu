#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Make the patched copy of the New World client that the emulator needs.

This tool writes a patched copy, ``BlightedNewWorld.exe``, next to the official
``NewWorld.exe`` (which it never modifies). The copy has four changes, and a
fifth one with ``--aws-proxy``:

1. Certificate bypass. The real client refuses the emulator's self-signed
   game-server certificate. The tool finds OpenSSL ``SSL_CTX_set_verify`` by a
   fixed byte signature and replaces its six-byte ``verify_mode`` write with
   ``NOP`` instructions. A fresh ``SSL_CTX`` starts with ``verify_mode`` 0
   (``SSL_VERIFY_NONE``), so no certificate is verified.

2. Exit to desktop. The Steam auth manager's ``EndSession`` only stores the quit
   callback and waits for the Omni persona client, which the emulator setup does
   not have, so "Exit to desktop" hangs. The Sonic auth manager's ``EndSession``
   runs the callback at once. The tool points the Steam ``EndSession`` vtable
   slot at the Sonic function (one 8-byte pointer in ``.rdata``). Both functions
   are found by byte signatures; the vtable slot is the single pointer to the
   Steam function.

3. Offline Steam ticket. The Steam auth manager asks the Steam client for a
   web API auth ticket and waits for a Steam callback. While the Steam client is
   offline that callback never comes, and the game stays at "press any button".
   Our ``/steam`` route does not read the ticket. The tool replaces the request
   in ``GetTicket`` with a direct call of the ticket handler with an empty ticket
   (handle 0, result OK, length 0), so the login goes on at once whether Steam is
   online or not. Both functions are found by byte signatures.

4. Typed chat. The client holds typed chat back until a privacy check (the
   console variable ``javelin.social.enable-vpc-restrictions``, default on)
   gets an answer from a platform service the emulator setup does not have, so
   nothing typed is ever sent. The settings function creates that variable
   with ``MOV R8B,1`` as its default; the tool makes the immediate 0. The site
   is found by the variable's name and the shape of its creation.

5. AWS proxy (``--aws-proxy``, for Windows). The remote-config (S3), analytics
   (Kinesis) and analytics-credentials (STS) clients call fixed Amazon names
   with certificate checks on. Windows has no per-process hosts or trust file,
   so each client is sent through the launcher's loopback proxy instead. After
   each client's ``Aws::Client::ClientConfiguration`` constructor call, a jump
   to a code cave in ``.text`` padding sets ``proxyHost`` to ``127.0.0.1``,
   ``proxyPort`` to the launcher's port and ``verifySSL`` to false, replays the
   instruction the jump replaced, and jumps back. The remote-config client
   sets ``verifySSL`` itself after that point, so the immediate of that write
   becomes 0 too. The launcher proxy checks the server's mirror certificate.

No change alters the file length. See ``docs/client-patch.md`` for the
procedure, the reasoning, and how to re-derive a signature if a build changes it.

Usage:
    scripts/patch-client.py <Bin64-directory> [--aws-proxy]
    scripts/patch-client.py <Bin64-directory> --verify [--aws-proxy]
    scripts/patch-client.py <Bin64-directory> --revert
"""

import argparse
import os
import re
import shutil
import struct
import sys
from typing import NoReturn

# SSL_CTX_set_verify(ctx, mode, callback):
#   MOV dword ptr [RCX + 0x158], EDX   89 91 58 01 00 00   <- the verify_mode write (patched out)
#   MOV qword ptr [RCX + 0x188], R8    4C 89 81 88 01 00 00
#   RET                                C3
SIGNATURE = bytes.fromhex("899158010000" "4c898188010000" "c3")
# The leading instruction we replace (the verify_mode write).
VERIFY_MODE_WRITE = bytes.fromhex("899158010000")
NOPS = b"\x90" * len(VERIFY_MODE_WRITE)

# Sonic auth manager EndSession (build 25810808: FUN_145a8d310): calls the quit callback, sets
# the flag at this+0x58, then frees the callback. The whole body has no address in it.
SONIC_END_SESSION = bytes.fromhex(
    "48895c2408" "4889742418" "4889542410" "57" "4883ec20" "488bfa" "488bd9"
    "488b02" "4c8b4008" "488d4a08" "41ffd0" "c6435801" "488b07" "4885c0" "741f"
    "488b00" "4885c0" "7410" "41b802000000" "488d5708" "488d4f08" "ffd0"
    "48c70700000000" "488b5c2430" "488b742440" "4883c420" "5f" "c3"
)
# Steam auth manager EndSession (build 25810808: FUN_145a8d380): stores the callback at
# this+0x30, calls the persona client at this+0x228 when it is set, and never runs the callback.
# The four bytes after E8 are the build-specific call offset.
STEAM_END_SESSION = re.compile(
    re.escape(bytes.fromhex("48895c2408" "4889542410" "57" "4883ec20" "488bfa" "488bd9" "4883c130"))
    + b"\xe8.{4}"
    + re.escape(bytes.fromhex("488b8b28020000" "4885c9" "7407" "488b01" "ff5030")),
    re.DOTALL,
)

# Offline Steam ticket patch. Four wildcard bytes are a RIP-relative or call offset.
REL32 = b".{4}"
# The Steam auth manager's ticket handle (HAuthTicket) and ticket length fields.
TICKET_HANDLE_FIELD = 0x2F0
TICKET_LENGTH_FIELD = 0x388
# The response the patched GetTicket hands to the handler at once (steam_api.h):
# k_HAuthTicketInvalid, k_EResultOK and an empty ticket. Our /steam route does not read the ticket.
NO_TICKET_HANDLE = 0
STEAM_RESULT_OK = 1
EMPTY_TICKET_LENGTH = 0
# GetTicket's epilogue: ADD RSP,0x20 / POP RBX / RET.
GET_TICKET_EPILOGUE = bytes.fromhex("4883c420" "5b" "c3")
# Steam auth manager GetTicket (build 25810808: FUN_145aa0d10). After it logs "Getting steam auth
# session ticket." (the request group) it cancels the old ticket (ISteamUser vtable +0x88,
# CancelAuthTicket), clears the old ticket, calls ISteamUser::GetAuthTicketForWebApi(NULL) (vtable
# +0x70) and stores the handle. The ticket comes later in a Steam callback, and only while the
# Steam client is connected to Steam.
STEAM_GET_TICKET = re.compile(
    re.escape(bytes.fromhex("4053" "4883ec20" "488bd9" "c681e802000001" "488d0d"))
    + REL32
    + re.escape(bytes.fromhex("488d15"))
    + REL32
    + b"\xe8"
    + REL32
    + b"(?P<request>"
    + re.escape(bytes.fromhex("83bb") + struct.pack("<I", TICKET_HANDLE_FIELD) + bytes.fromhex("00" "743d" "488d0d"))
    + REL32
    + re.escape(bytes.fromhex("ff15"))
    + REL32
    + re.escape(
        bytes.fromhex("8b93") + struct.pack("<I", TICKET_HANDLE_FIELD)
        + bytes.fromhex("488b08" "488b01" "ff9088000000" "488d8b8c030000" "33d2" "41b8000a0000" "e8")
    )
    + REL32
    + re.escape(bytes.fromhex("c783") + struct.pack("<II", TICKET_LENGTH_FIELD, EMPTY_TICKET_LENGTH) + bytes.fromhex("488d0d"))
    + REL32
    + re.escape(bytes.fromhex("ff15"))
    + REL32
    + re.escape(
        bytes.fromhex("33d2" "488b08" "488b01" "ff5070" "8983") + struct.pack("<I", TICKET_HANDLE_FIELD)
        + GET_TICKET_EPILOGUE
    )
    + b")",
    re.DOTALL,
)
# The Steam auth manager's GetTicketForWebApiResponse_t handler (build 25810808: FUN_145a94820,
# "OnAuthSessionTicket"; the Steam callback object at this+0x1A8 enters it through the thunk at
# 0x145A9F750). RCX is the manager, RDX the response. It reads the handle (+0), the result (+4)
# and the ticket length (+8), and copies that many ticket bytes from +12. A handle other than the
# stored one is ignored; a result other than OK reports a Steam error; then it calls our /steam
# route with the hex ticket.
ON_AUTH_SESSION_TICKET = re.compile(
    re.escape(
        bytes.fromhex(
            "48895c2410" "4889742418" "48897c2420" "55" "4154" "4155" "4156" "4157"
            "488dac2450faffff" "4881ecb0060000" "488bf2" "488bd9" "4533ff" "4489bde0050000"
            "448b32" "8b7a04" "4488bde0050000" "488d05"
        )
    )
    + REL32
    + re.escape(bytes.fromhex("4889442440" "44897c2448" "0f28442440" "660f7f442450" "4c8d05"))
    + REL32
    + re.escape(bytes.fromhex("488d542450" "488d8de0050000" "e8"))
    + REL32
    + re.escape(bytes.fromhex("b802000000" "4438bde0050000" "0f45f8" "488d0d"))
    + REL32
    + re.escape(bytes.fromhex("443bb3") + struct.pack("<I", TICKET_HANDLE_FIELD) + bytes.fromhex("7411")),
    re.DOTALL,
)
# The response goes in GetTicket's home space: after PUSH RBX / SUB RSP,0x20 it is [RSP+0x30], which
# the handler (it saves registers in its own home space, [RSP+0x08] to [RSP+0x20] here) never uses.
TICKET_RESPONSE_STACK = 0x30
MOV_DWORD_RBX = bytes.fromhex("c783")
MOV_DWORD_RSP8 = bytes.fromhex("c74424")
LEA_RDX_RSP8 = bytes.fromhex("488d5424")
MOV_RCX_RBX = bytes.fromhex("488bcb")

# Typed chat patch. The settings function (build 25810808: FUN_14616d7b0) creates each console
# variable as LEA RDX,[name] / LEA RCX,[RBP+var] / CALL / MOV R8B,<default> / LEA RDX,[RBP+var].
CHAT_RESTRICTION_VARIABLE = b"javelin.social.enable-vpc-restrictions"
NUL = b"\x00"
LEA_RDX_RIP = bytes.fromhex("488d15")
VARIABLE_CREATION = re.compile(
    re.escape(LEA_RDX_RIP)
    + b"(?P<name>.{4})"
    + re.escape(bytes.fromhex("488d8d"))
    + b"(?P<variable>.{4})"
    + b"\xe8.{4}"
    + re.escape(bytes.fromhex("41b0"))
    + b"(?P<default>\x01)"
    + re.escape(bytes.fromhex("488d95"))
    + b"(?P=variable)",
    re.DOTALL,
)
LEA_RDX_RIP_LEN = 7
DEFAULT_OFF = b"\x00"

# AWS proxy patch (docs/re/2026-09-20-windows-launcher-network.md). The launcher's loopback proxy
# listens here; bins/launcher/src/aws_proxy.rs holds the same port.
AWS_PROXY_HOST = b"127.0.0.1"
AWS_PROXY_PORT = 47443
# Aws::Client::ClientConfiguration field offsets (constructor FUN_147467a40; the WinHTTP client
# FUN_147491570 reads proxyHost at +0xA0, proxyPort at +0xC0, verifySSL at +0x1B8). proxyHost is
# an MSVC std::string: a 16-byte inline buffer, its size at +0x10, its capacity (15) at +0x18.
CONFIG_PROXY_HOST = 0xA0
CONFIG_PROXY_HOST_SIZE = 0xB0
INLINE_STRING_CAPACITY = 15
CONFIG_PROXY_PORT = 0xC0
CONFIG_VERIFY_SSL = 0x1B8
# The ClientConfiguration constructor (build 25810808: FUN_147467a40). It returns the config in
# RAX. The four bytes after E8 are the build-specific call offset.
CONFIG_CONSTRUCTOR = re.compile(
    re.escape(bytes.fromhex("48894c2408" "53" "56" "57" "4883ec30" "488bf9" "33f6"))
    + b"\xe8.{4}"
    + re.escape(bytes.fromhex("90" "c7472001000000")),
    re.DOTALL,
)
CALL_OPCODE = 0xE8
JUMP_OPCODE = b"\xe9"
NOP = b"\x90"
CALL_LEN = 5
JUMP_LEN = 5
# Each AWS client set-up: the bytes before the constructor call, the instruction after its NOP
# (replayed in the cave), and the bytes after that. The jump replaces the NOP and that instruction.
AWS_CONFIG_SITES = (
    # S3 remote-config client (build 25810808: FUN_140853a80). LEA RCX,[RSP+0x50] / CALL / NOP /
    # MOV byte ptr [RSP+0xAC],R15B / MOV RDX,[RIP+..]
    ("remote-config proxy", "488d4c2450", "4488bc24ac000000", "488b15"),
    # Kinesis analytics client (build 25810808: FUN_1461c3cc0). LEA RCX,[RBP-0x10] / CALL / NOP /
    # MOV byte ptr [RBP+0x4C],0 / MOV dword ptr [RBP+0x10],1
    ("analytics proxy", "488d4df0", "c6454c00", "c7451001000000"),
    # STS client of the analytics credentials (build 25810808: FUN_1461c5220). LEA RCX,[RBP+0x10] /
    # CALL / NOP / MOV byte ptr [RBP+0x6C],0 / LEA RAX,[RSP+0x30]
    ("analytics credentials proxy", "488d4d10", "c6456c00", "488d442430"),
)
# The S3 set-up writes verifySSL = 1 after the constructor (MOV byte ptr [RSP+0x208],1, then
# MOV dword ptr [RSP+0x70],1). The patch makes the immediate 0.
S3_VERIFY_SSL_WRITE = bytes.fromhex("c684240802000001" "c744247001000000")
S3_VERIFY_SSL_IMMEDIATE = 7
VERIFY_SSL_OFF = b"\x00"
# Code caves go in the first run of INT3 padding this long in the code section, one per site.
INT3 = 0xCC
CAVE_RUN_MIN = 256
CAVE_SLOT = 0x50
CAVE_ALIGN = 16
# Field writes through RAX: MOV dword/word/qword/byte ptr [RAX+disp32], imm.
MOV_DWORD_RAX = bytes.fromhex("c780")
MOV_QWORD_RAX = bytes.fromhex("48c780")
MOV_BYTE_RAX = bytes.fromhex("c680")
DWORD_LEN = 4
POINTER_LEN = 8

OFFICIAL_NAME = "NewWorld.exe"
BACKUP_NAME = "NewWorld.exe.orig"
PATCHED_NAME = "BlightedNewWorld.exe"

SEE_DOC = "see docs/client-patch.md"

# PE layout.
PE_OFFSET_FIELD = 0x3C
FILE_HEADER_LEN = 24
SECTION_COUNT_FIELD = 6
OPTIONAL_HEADER_SIZE_FIELD = 20
IMAGE_BASE_FIELD = 24
SECTION_HEADER_LEN = 40
SECTION_FIELDS_OFFSET = 8
SECTION_CHARACTERISTICS_OFFSET = 36
IMAGE_SCN_MEM_EXECUTE = 0x20000000


def fail(message: str) -> NoReturn:
    sys.exit(f"error: {message} ({SEE_DOC})")


def find_unique_match(data: bytes, pattern: "bytes | re.Pattern[bytes]", what: str) -> "re.Match[bytes]":
    """The single match of `pattern`, or exit with an error."""
    if isinstance(pattern, bytes):
        pattern = re.compile(re.escape(pattern), re.DOTALL)
    matches = list(pattern.finditer(data))
    if not matches:
        fail(f"{what} signature not found; the client build may have changed")
    if len(matches) > 1:
        fail(f"{what} signature found more than once; aborting")
    return matches[0]


def find_unique(data: bytes, pattern: "bytes | re.Pattern[bytes]", what: str) -> int:
    """Offset of the single match of `pattern`, or exit with an error."""
    return find_unique_match(data, pattern, what).start()


def sections(data: bytes) -> "list[tuple[int, int, int, int, int]]":
    """(image base + RVA, raw start, raw size, virtual size, characteristics) of every section."""
    try:
        pe = struct.unpack_from("<I", data, PE_OFFSET_FIELD)[0]
        count = struct.unpack_from("<H", data, pe + SECTION_COUNT_FIELD)[0]
        optional_size = struct.unpack_from("<H", data, pe + OPTIONAL_HEADER_SIZE_FIELD)[0]
        image_base = struct.unpack_from("<Q", data, pe + FILE_HEADER_LEN + IMAGE_BASE_FIELD)[0]
        header = pe + FILE_HEADER_LEN + optional_size
        table = []
        for index in range(count):
            entry = header + index * SECTION_HEADER_LEN
            virtual_size, rva, raw_size, raw_start = struct.unpack_from(
                "<IIII", data, entry + SECTION_FIELDS_OFFSET
            )
            characteristics = struct.unpack_from("<I", data, entry + SECTION_CHARACTERISTICS_OFFSET)[0]
            table.append((image_base + rva, raw_start, raw_size, virtual_size, characteristics))
        return table
    except struct.error:
        fail("not a readable PE file")


def virtual_address(data: bytes, file_offset: int) -> int:
    """The virtual address of `file_offset` in the PE image `data`, or exit with an error."""
    for address, raw_start, raw_size, _, _ in sections(data):
        if raw_start <= file_offset < raw_start + raw_size:
            return address + file_offset - raw_start
    fail(f"file offset 0x{file_offset:X} is in no section")


def call_target(data: bytes, call: int) -> int:
    """The virtual address a CALL rel32 at file offset `call` goes to."""
    return virtual_address(data, call) + CALL_LEN + struct.unpack_from("<i", data, call + 1)[0]


def rel32(source: int, length: int, target: int) -> bytes:
    """The rel32 of a `length`-byte instruction at `source` that goes to `target`."""
    return struct.pack("<i", target - (source + length))


def cave_start(data: bytes) -> int:
    """File offset of the first aligned byte of the first INT3 run of CAVE_RUN_MIN bytes that lies
    in the loaded part of a code section, or exit with an error."""
    run = re.compile(re.escape(bytes([INT3])) + b"{%d,}" % CAVE_RUN_MIN, re.DOTALL)
    for address, raw_start, raw_size, virtual_size, characteristics in sections(data):
        if not characteristics & IMAGE_SCN_MEM_EXECUTE:
            continue
        end = raw_start + min(raw_size, virtual_size)
        for match in run.finditer(data, raw_start, end):
            start = match.start() + (-(address + match.start() - raw_start)) % CAVE_ALIGN
            if match.end() - start >= CAVE_SLOT * len(AWS_CONFIG_SITES):
                return start
    fail("no INT3 padding for the AWS proxy code caves")


def config_cave(cave: int, replayed: bytes, resume: int) -> bytes:
    """Code at VA `cave`, entered by a jump right after a ClientConfiguration constructor call (the
    config in RAX): sets the proxy host, the proxy port and verifySSL = false, runs `replayed`,
    and jumps to VA `resume`."""
    host = AWS_PROXY_HOST + b"\x00"
    host += b"\x00" * (-len(host) % DWORD_LEN)
    code = bytearray()
    for index in range(0, len(host), DWORD_LEN):
        code += MOV_DWORD_RAX + struct.pack("<I", CONFIG_PROXY_HOST + index)
        code += host[index : index + DWORD_LEN]
    code += MOV_QWORD_RAX + struct.pack("<II", CONFIG_PROXY_HOST_SIZE, len(AWS_PROXY_HOST))
    code += MOV_DWORD_RAX + struct.pack("<II", CONFIG_PROXY_PORT, AWS_PROXY_PORT)
    code += MOV_BYTE_RAX + struct.pack("<I", CONFIG_VERIFY_SSL) + VERIFY_SSL_OFF
    code += replayed
    code += JUMP_OPCODE + rel32(cave + len(code), JUMP_LEN, resume)
    if len(code) > CAVE_SLOT:
        fail("an AWS proxy code cave is larger than its slot")
    return bytes(code)


def plan_aws_proxy(data: bytes) -> "list[tuple[str, int, bytes]]":
    """The edits that send the S3, Kinesis and STS clients through the launcher's proxy."""
    if len(AWS_PROXY_HOST) > INLINE_STRING_CAPACITY:
        fail("the AWS proxy host does not fit the inline string buffer")
    constructor = virtual_address(data, find_unique(data, CONFIG_CONSTRUCTOR, "ClientConfiguration"))
    edits = []
    cave = cave_start(data)
    for name, lead, replayed_hex, trail in AWS_CONFIG_SITES:
        lead, replayed = bytes.fromhex(lead), bytes.fromhex(replayed_hex)
        pattern = re.compile(
            re.escape(lead) + b"\xe8.{4}" + re.escape(NOP + replayed + bytes.fromhex(trail)),
            re.DOTALL,
        )
        calls = [
            match.start() + len(lead)
            for match in pattern.finditer(data)
            if call_target(data, match.start() + len(lead)) == constructor
        ]
        if len(calls) != 1:
            fail(f"{name} signature found {len(calls)} times; the client build may have changed")
        site = calls[0] + CALL_LEN
        replaced = len(NOP) + len(replayed)
        site_va, cave_va = virtual_address(data, site), virtual_address(data, cave)
        jump = JUMP_OPCODE + rel32(site_va, JUMP_LEN, cave_va)
        edits.append((name, site, jump + NOP * (replaced - JUMP_LEN)))
        edits.append((f"{name} cave", cave, config_cave(cave_va, replayed, site_va + replaced)))
        cave += CAVE_SLOT
    verify_ssl = find_unique(data, S3_VERIFY_SSL_WRITE, "remote-config verifySSL")
    edits.append(("remote-config verifySSL", verify_ssl + S3_VERIFY_SSL_IMMEDIATE, VERIFY_SSL_OFF))
    return edits


def plan_offline_ticket(data: bytes) -> "list[tuple[str, int, bytes]]":
    """The edit that makes GetTicket hand an empty ticket to its handler at once, instead of
    asking the Steam client and waiting for a callback that never comes while Steam is offline."""
    request = find_unique_match(data, STEAM_GET_TICKET, "Steam GetTicket")
    handler = virtual_address(data, find_unique(data, ON_AUTH_SESSION_TICKET, "Steam OnAuthSessionTicket"))
    site = request.start("request")
    length = request.end("request") - site
    code = MOV_DWORD_RBX + struct.pack("<II", TICKET_HANDLE_FIELD, NO_TICKET_HANDLE)
    response = (NO_TICKET_HANDLE, STEAM_RESULT_OK, EMPTY_TICKET_LENGTH)
    for index, value in enumerate(response):
        code += MOV_DWORD_RSP8 + bytes([TICKET_RESPONSE_STACK + index * DWORD_LEN])
        code += struct.pack("<I", value)
    code += LEA_RDX_RSP8 + bytes([TICKET_RESPONSE_STACK]) + MOV_RCX_RBX
    call = virtual_address(data, site) + len(code)
    code += bytes([CALL_OPCODE]) + rel32(call, CALL_LEN, handler) + GET_TICKET_EPILOGUE
    if len(code) > length:
        fail("the offline Steam ticket code is larger than the request it replaces")
    return [("offline Steam ticket", site, code + bytes([INT3]) * (length - len(code)))]


def plan_typed_chat(data: bytes) -> "list[tuple[str, int, bytes]]":
    """The edit that turns the typed chat privacy check off: the default of
    ``javelin.social.enable-vpc-restrictions`` becomes 0."""
    name = virtual_address(
        data, find_unique(data, NUL + CHAT_RESTRICTION_VARIABLE + NUL, "chat restriction name") + len(NUL)
    )
    sites = [
        match.start("default")
        for match in VARIABLE_CREATION.finditer(data)
        if virtual_address(data, match.start()) + LEA_RDX_RIP_LEN
        + struct.unpack("<i", match.group("name"))[0] == name
    ]
    if len(sites) != 1:
        fail(f"chat restriction site found {len(sites)} times; the client build may have changed")
    return [("typed chat", sites[0], DEFAULT_OFF)]


def plan_patches(data: bytes, aws_proxy: bool) -> "list[tuple[str, int, bytes]]":
    """The (name, file offset, new bytes) edits that make the patched copy from `data`; with
    `aws_proxy`, also the AWS proxy edits."""
    verify_site = find_unique(data, SIGNATURE, "certificate-verify")
    sonic = virtual_address(data, find_unique(data, SONIC_END_SESSION, "Sonic EndSession"))
    steam = virtual_address(data, find_unique(data, STEAM_END_SESSION, "Steam EndSession"))
    slot = find_unique(data, struct.pack("<Q", steam), "Steam EndSession vtable slot")
    edits = [
        ("certificate bypass", verify_site, NOPS),
        ("exit to desktop", slot, struct.pack("<Q", sonic)),
        *plan_offline_ticket(data),
        *plan_typed_chat(data),
    ]
    return edits + plan_aws_proxy(data) if aws_proxy else edits


def read_file(path: str) -> bytes:
    with open(path, "rb") as handle:
        return handle.read()


def is_patched(original: bytes, data: bytes, aws_proxy: bool) -> bool:
    """Whether `data` is `original` with every planned edit."""
    if len(data) != len(original):
        return False
    return all(
        data[site : site + len(new)] == new
        for _, site, new in plan_patches(original, aws_proxy)
    )


def make_patched(official: str, backup: str, patched: str, aws_proxy: bool) -> None:
    """Back up the official exe once and write the patched copy from its bytes."""
    original = read_file(official)
    edits = plan_patches(original, aws_proxy)
    for name, site, _ in edits:
        print(f"{name}: patch site at file offset 0x{site:X}")

    if not os.path.exists(backup):
        shutil.copy2(official, backup)
        print(f"backup written: {backup}")
    else:
        print(f"backup kept: {backup}")

    buffer = bytearray(original)
    for _, site, new in edits:
        buffer[site : site + len(new)] = new
    temporary = patched + ".tmp"
    with open(temporary, "wb") as handle:
        handle.write(buffer)
        handle.flush()
        os.fsync(handle.fileno())
    shutil.copymode(official, temporary)
    os.replace(temporary, patched)

    if not is_patched(original, read_file(patched), aws_proxy):
        sys.exit("error: verification of the patched copy failed")
    print(f"wrote {patched} ({', '.join(name for name, _, _ in edits)})")


def verify_patched(official: str, patched: str, aws_proxy: bool) -> int:
    """Return 0 when the patched copy is present and correctly patched, else 1."""
    if not os.path.isfile(patched):
        print(f"missing: {patched}")
        return 1
    good = is_patched(read_file(official), read_file(patched), aws_proxy)
    print("patched copy:", "OK" if good else "WRONG")
    return 0 if good else 1


def revert(patched: str) -> None:
    if os.path.exists(patched):
        os.remove(patched)
        print(f"removed: {patched}")
    else:
        print("nothing to remove")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Make BlightedNewWorld.exe, the patched client copy the emulator needs."
    )
    parser.add_argument("bin64", help="the game Bin64 directory that holds NewWorld.exe")
    group = parser.add_mutually_exclusive_group()
    group.add_argument(
        "--verify", action="store_true", help="only check the patched copy; write nothing"
    )
    group.add_argument(
        "--revert", action="store_true", help="delete the patched copy (keeps the official exe)"
    )
    parser.add_argument(
        "--aws-proxy",
        action="store_true",
        help="also send the remote-config, Kinesis and STS clients through the launcher's "
        "loopback proxy (Windows)",
    )
    arguments = parser.parse_args()

    official = os.path.join(arguments.bin64, OFFICIAL_NAME)
    backup = os.path.join(arguments.bin64, BACKUP_NAME)
    patched = os.path.join(arguments.bin64, PATCHED_NAME)

    if arguments.revert:
        revert(patched)
        return
    if not os.path.isfile(official):
        sys.exit(f"error: not found: {official}")
    if arguments.verify:
        sys.exit(verify_patched(official, patched, arguments.aws_proxy))
    make_patched(official, backup, patched, arguments.aws_proxy)


if __name__ == "__main__":
    main()
