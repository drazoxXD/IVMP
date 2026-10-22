# DLC: the episodes' content in the GTA IV world

`dlc` in `server.cfg` (and in the client's `ivmp.json`): 1 = GTA IV, 2 = EFLC, 3 = both (default 3).
With 2 or 3 the client loads the vehicles, peds and weapons of The Lost and Damned (TLAD) and The
Ballad of Gay Tony (TBoGT) into the base game's world. The game does not do this itself: it runs one
episode at a time, and its tables are sized for one.

Status: vehicles load and spawn (`spawn car hexer`, `slamvan`, `hakuchou`, `double`). Not tried or
not done: vehicle sounds, peds and weapons in play, the server checking that clients agree on `dlc`,
and the setup2.xml flags below without editing files.

## How the game loads an episode

Each episode folder (`TLAD`, `TBoGT`) has `setup2.xml` (its content blocks: radio, the episode),
`content.dat` (its data files by kind: IDE, HANDLING, CARCOLS, IMGLIST, IPL, ...) and
`common/data/images.txt` (its IMG archives). The episode's block in `setup2.xml` has
`<!-- <enabled /> -->`: commented out, the game only reads the episode when it is the active one. With
the comment removed (`<enabled />`) the game reads `content.dat` of both episodes at start, next to
the base game's data. This is the switch that mixes the content. Right now it is edited by hand
(backups: `setup2.xml.ivmp-backup`), the plan is to patch the check in memory.

## What had to be fixed (each was a crash)

1. **Too much content.** The episodes' `content.dat` and `images.txt` hold their whole maps,
   interiors, cutscene animations and scripts: they overflow the streaming tables and would overwrite
   the base map. `Hooks/src/dlc.rs` hooks the game's `fopen` and line reader and drops those lines: what
   stays is the data of vehicles, peds and weapons and the model archives (`pc/models/cdimages/`).
2. **Model pools.** Every kind of model definition is a fixed pool `{capacity, used, base}`, 12 bytes
   each in a table at 0x0103AD64, built at the start from the capacity in the data section. The base
   game nearly fills several. `boot::raise_model_pools` raises 23 of them (x3, x2 for the big ones)
   right after the DLL attaches, before the game builds them. The 28000 base model infos are left
   alone (the model table has 31000 slots).
3. **Handling table.** `handling.dat` fills a static table of 160 entries (0x016DD6A0, 0x110 bytes
   each; the game uses 125, the episodes add about 41). `dlc::relocate_handling` moves it to a buffer
   twice the size and patches the address operands that point into it. These are found by decoding
   the code with iced-x86 (`ivmp_core::hook::find_address_operands`); an early version searched the raw
   bytes, which also matches bytes of other instructions and corrupted code. The loader's append
   function (0x00C73FC0) is hooked so a new entry gets the three category pointers (boat, bike,
   flying) the game's own entries carry, or the first `$` / `!` / `%` line of a new vehicle writes
   through a null pointer.

## Finding the next limit

Put a `dlctrace.txt` next to `IVMP_Client.dll`: the log then has a `datfile:` line for every file the
game's data loader opens and for every line that is dropped. The crash address in the log is a game
address, `ghidra 0x...` is the address in the Ghidra project.

Other tables that may still be too small (not hit yet): the stream stores (textures 6000, drawable
dictionaries 2304, ...), created in code with their capacity as an immediate; the 31000 model slots;
vehicle audio.
