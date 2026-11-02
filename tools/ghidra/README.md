# Ghidra tools

These scripts read your own installed New World client with Ghidra. They write nothing into
this repository. Every output (decompiled functions, name tables, string lists) is client data,
so put it in the ignored `local-data/` folder.

## Scripts

The folder `scripts/` holds Ghidra Java scripts. Run them headless with
`-scriptPath tools/ghidra/scripts`. An example command is in
`docs/re/2026-09-14-can-take-items.md`. All addresses are hex.

| Script | Arguments | What it does |
|---|---|---|
| `DecompileAt.java` | address, optional output file | Decompiles the function that holds the address. |
| `XrefDecompile.java` | function entry address, optional output file | Lists the references to the function and decompiles each caller. |
| `FindAndDecompile.java` | text, optional output file | Lists every place of the text in the loaded memory, lists the references to its first place, and decompiles each function that refers to it. |
| `FindStr.java` | text | Lists every place of the text and the references to each place. |
| `XrefList.java` | address | Lists the references to the address and the function of each one. |
| `DumpAt.java` | address, count | Prints that many 8-byte values from the address, and names the function or data that each value points to. |
| `SymAt.java` | one or more addresses | Prints the symbol at each address, and the nearest symbol with `vtable` in its name in the 64 slots before it. |
| `VtableOwner.java` | one or more function addresses | For each reference to the function, looks back up to 64 slots for a pointer to a `typeinfo` symbol (the RTTI of the vtable). |
| `SweepProbe.java` | output folder | Prints the program name, the image base and the function count, and writes them to `meta.json` in the folder. |
| `FullSweep.java` | output folder, start index, count, threads | Decompiles one batch of the full function sweep. It reads `sweep-config.json` in the output folder. |

The scripts with an optional output file print to the Ghidra log when you give no file.

## Full function sweep

The folder `sweep/` holds the full function sweep
(`docs/re/2026-09-16-full-function-sweep.md`). `sweep.sh` runs `FullSweep.java` in batches
until every function is written. It needs `GHIDRA_DIR` (the Ghidra install) and
`NW_GHIDRA_PROJECT_DIR` (the analysed project). `NW_GHIDRA_PROJECT_NAME` sets the project name
(default `NewWorld25810808`). The output goes to `local-data/re-sweep/` unless
`NW_SWEEP_OUTDIR` is set. The type names come from `$NW_GAME_DIR/typeindex.json`. To check the
name rules, run `python3 -m unittest test_rules` in `sweep/`.
